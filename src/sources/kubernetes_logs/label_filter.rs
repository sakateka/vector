//! Compiled filters for Kubernetes label keys.

use std::collections::BTreeMap;

use glob::{Pattern, PatternError};
use vector_lib::configurable::configurable_component;

/// Filters applied independently to the labels cached from each Kubernetes object.
#[configurable_component]
#[configurable(metadata(docs::examples = "example_label_filters()"))]
#[derive(Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub struct LabelFilters {
    /// Filter for Pod label keys.
    pub pod: LabelFilter,

    /// Filter for Namespace label keys.
    pub namespace: LabelFilter,

    /// Filter for Node label keys.
    pub node: LabelFilter,
}

fn example_label_filters() -> serde_json::Value {
    serde_json::json!({
        "pod": {
            "includes": ["app.kubernetes.io/*"],
            "excludes": ["app.kubernetes.io/version"]
        },
        "namespace": {"includes": []},
        "node": {"includes": ["topology.kubernetes.io/zone"]}
    })
}

/// Selects label keys before Kubernetes objects are cached and their labels enrich events.
///
/// Filters prune only cached labels. Annotations, other object fields, object discovery, and
/// server-side selectors are unchanged. Existing event fields keep the normal annotation behavior:
/// selected labels can overwrite the same destination keys, and excluded labels are not inserted.
#[configurable_component]
#[derive(Clone, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub struct LabelFilter {
    /// Label key patterns to include.
    ///
    /// Omit this option or set it to `null` to include all keys before exclusions.
    /// An empty list selects no labels. Overlapping patterns select each key once.
    #[configurable(metadata(docs::examples = "app.kubernetes.io/*"))]
    #[configurable(metadata(docs::examples = "topology.kubernetes.io/zone"))]
    includes: Option<Vec<LabelPattern>>,

    /// Label key patterns to exclude.
    ///
    /// Exclusions take precedence over inclusions. Omit this option or use an empty list to
    /// exclude no keys.
    #[configurable(metadata(docs::examples = "cpu-feature.node.kubevirt.io/*"))]
    excludes: Option<Vec<LabelPattern>>,
}

impl LabelFilter {
    pub(super) fn retain(&self, labels: &mut Option<BTreeMap<String, String>>) {
        // The default keeps every label without walking the map.
        if self.includes.is_none() && self.excludes.as_ref().is_none_or(Vec::is_empty) {
            return;
        }
        if let Some(labels) = labels {
            labels.retain(|key, _| self.contains(key));
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.includes
            .as_ref()
            .is_none_or(|patterns| patterns.iter().any(|pattern| pattern.0.matches(key)))
            && !self
                .excludes
                .as_ref()
                .is_some_and(|patterns| patterns.iter().any(|pattern| pattern.0.matches(key)))
    }
}

/// A compiled glob pattern for an entire label key.
#[configurable_component]
#[derive(Clone, Debug)]
#[serde(try_from = "String", into = "String")]
struct LabelPattern(Pattern);

impl TryFrom<String> for LabelPattern {
    type Error = PatternError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Pattern::new(&value).map(Self)
    }
}

impl From<LabelPattern> for String {
    fn from(pattern: LabelPattern) -> Self {
        pattern.0.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{StreamExt as _, TryStreamExt as _, channel::mpsc};
    use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::ObjectMeta};
    use kube::runtime::{
        WatchStreamExt as _,
        reflector::{ObjectRef, store::Writer},
        watcher::{Error, Event},
    };
    use similar_asserts::assert_eq;
    use tokio::sync::oneshot;

    use super::*;
    use crate::kubernetes::{custom_reflector, meta_cache::MetaCache};

    #[test]
    fn retain_preserves_absent_labels_and_default_maps() {
        for yaml in ["{}", "includes: null", "excludes: []", "includes: []"] {
            let filter: LabelFilter = serde_yaml::from_str(yaml).unwrap();
            let mut labels = None;
            filter.retain(&mut labels);
            assert_eq!(labels, None);
        }
        for yaml in ["{}", "includes: null", "excludes: []"] {
            let filter: LabelFilter = serde_yaml::from_str(yaml).unwrap();
            let mut labels = Some([("app/name".to_owned(), "api".to_owned())].into());
            let expected = labels.clone();
            filter.retain(&mut labels);
            assert_eq!(labels, expected);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Exercises initialization, updates, relisting, and delayed deletion as one lifecycle."
    )]
    #[tokio::test(start_paused = true)]
    async fn watch_objects_are_pruned_before_storage_and_delayed_deletion() {
        let filter: LabelFilter = serde_yaml::from_str("includes: ['keep*']").unwrap();
        let pod = Pod {
            metadata: ObjectMeta {
                name: Some("pod".to_owned()),
                namespace: Some("ns".to_owned()),
                uid: Some("uid".to_owned()),
                resource_version: Some("1".to_owned()),
                labels: Some(
                    [
                        ("keep/name".to_owned(), "api".to_owned()),
                        ("keep/remove".to_owned(), "old".to_owned()),
                        ("drop/name".to_owned(), "discard".to_owned()),
                    ]
                    .into(),
                ),
                annotations: Some([("drop/name".to_owned(), "keep annotation".to_owned())].into()),
                ..ObjectMeta::default()
            },
            ..Pod::default()
        };
        let mut expected = pod.clone();
        expected
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .remove("drop/name");
        let key = ObjectRef::from_obj(&pod);
        let writer = Writer::default();
        let reader = writer.as_reader();
        let (tx, mut rx) = mpsc::unbounded::<(_, oneshot::Sender<()>)>();
        let send = |event| {
            let (processed, done) = oneshot::channel();
            tx.unbounded_send((event, processed)).unwrap();
            async move {
                tokio::time::timeout(Duration::from_secs(1), done)
                    .await
                    .unwrap()
                    .unwrap();
            }
        };
        let stream = async_stream::stream! {
            while let Some((event, processed)) = rx.next().await {
                yield event;
                // The reflector polls again only after processing the preceding event.
                processed.send(()).unwrap();
            }
        }
        .modify(move |pod: &mut Pod| filter.retain(&mut pod.metadata.labels))
        .inspect_ok(move |event| {
            // Check payloads before either reflector storage or its delayed-delete buffer.
            if let Event::Apply(pod) | Event::InitApply(pod) | Event::Delete(pod) = event {
                assert!(
                    pod.metadata
                        .labels
                        .as_ref()
                        .unwrap()
                        .keys()
                        .all(|key| key.starts_with("keep"))
                );
            }
        });
        let task = tokio::spawn(custom_reflector(
            writer,
            MetaCache::new(),
            stream,
            Duration::from_secs(10),
        ));
        send(Ok(Event::Init)).await;
        send(Ok(Event::InitApply(pod.clone()))).await;
        assert!(reader.get(&key).is_none());
        send(Ok(Event::InitDone)).await;
        assert_eq!(reader.get(&key).as_deref(), Some(&expected));
        send(Err(Error::NoResourceVersion)).await;

        // A live update replaces values, adds labels, and removes labels no longer present.
        let mut updated = pod.clone();
        updated.metadata.resource_version = Some("2".to_owned());
        updated.metadata.labels = Some(
            [
                ("keep/name".to_owned(), "updated".to_owned()),
                ("keep/new".to_owned(), "new".to_owned()),
                ("drop/new".to_owned(), "discard".to_owned()),
            ]
            .into(),
        );
        expected = updated.clone();
        expected
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .remove("drop/new");
        send(Ok(Event::Apply(updated.clone()))).await;
        assert_eq!(reader.get(&key).as_deref(), Some(&expected));

        send(Ok(Event::Delete(updated.clone()))).await;
        assert_eq!(reader.get(&key).as_deref(), Some(&expected));
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while reader.get(&key).is_some() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        send(Ok(Event::Apply(updated.clone()))).await;
        assert_eq!(reader.get(&key).as_deref(), Some(&expected));

        // A new application cancels the effect of an already queued deletion.
        send(Ok(Event::Delete(updated.clone()))).await;
        send(Ok(Event::Apply(updated.clone()))).await;
        tokio::time::sleep(Duration::from_secs(11)).await;
        assert_eq!(reader.get(&key).as_deref(), Some(&expected));

        // A relist replaces old labels, and a subsequent empty relist removes the object.
        updated
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("keep/name".to_owned(), "relisted".to_owned());
        expected
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("keep/name".to_owned(), "relisted".to_owned());
        for event in [Event::Init, Event::InitApply(updated), Event::InitDone] {
            send(Ok(event)).await;
        }
        assert_eq!(reader.get(&key).as_deref(), Some(&expected));
        send(Ok(Event::Init)).await;
        send(Ok(Event::InitDone)).await;
        assert!(reader.get(&key).is_none());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}
