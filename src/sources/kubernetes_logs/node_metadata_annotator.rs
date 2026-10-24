//! Annotates events with node metadata.

#![deny(missing_docs)]

use std::collections::BTreeSet;

use itertools::Either;
use k8s_openapi::{api::core::v1::Node, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::runtime::reflector::{ObjectRef, store::Store};
use vector_lib::{
    config::{LegacyKey, LogNamespace},
    configurable::configurable_component,
    lookup::{
        OwnedTargetPath,
        lookup_v2::{OptionalTargetPath, ValuePath},
        owned_value_path, path,
    },
};

use super::Config;
use crate::event::{Event, LogEvent};

/// Configuration for how the events are enriched with Node metadata.
#[configurable_component]
#[derive(Clone, Debug)]
#[serde(deny_unknown_fields, default)]
pub struct FieldsSpec {
    /// Event field for the Node's labels.
    ///
    /// Set to `""` to suppress this key.
    #[configurable(metadata(docs::examples = ".k8s.node_labels"))]
    #[configurable(metadata(docs::examples = "k8s.node_labels"))]
    #[configurable(metadata(docs::examples = ""))]
    pub node_labels: OptionalTargetPath,

    /// Node label keys to include when enriching events.
    ///
    /// When unset, all Node labels are included. An empty list includes none.
    /// Keys are matched exactly, including dots and slashes.
    pub node_labels_include: Option<BTreeSet<String>>,
}

impl Default for FieldsSpec {
    fn default() -> Self {
        Self {
            node_labels: OwnedTargetPath::event(owned_value_path!("kubernetes", "node_labels"))
                .into(),
            node_labels_include: None,
        }
    }
}

/// Annotate the event with node metadata.
pub struct NodeMetadataAnnotator {
    node_state_reader: Store<Node>,
    fields_spec: FieldsSpec,
    log_namespace: LogNamespace,
}

impl NodeMetadataAnnotator {
    /// Create a new [`NodeMetadataAnnotator`].
    pub const fn new(
        node_state_reader: Store<Node>,
        fields_spec: FieldsSpec,
        log_namespace: LogNamespace,
    ) -> Self {
        Self {
            node_state_reader,
            fields_spec,
            log_namespace,
        }
    }
}

impl NodeMetadataAnnotator {
    /// Annotates an event with the information from the [`Node::metadata`].
    pub fn annotate(&self, event: &mut Event, node: &str) -> Option<()> {
        let log = event.as_mut_log();
        let obj = ObjectRef::<Node>::new(node);
        let resource = self.node_state_reader.get(&obj)?;
        let node: &Node = resource.as_ref();

        annotate_from_metadata(log, &self.fields_spec, &node.metadata, self.log_namespace);
        Some(())
    }
}

fn annotate_from_metadata(
    log: &mut LogEvent,
    fields_spec: &FieldsSpec,
    metadata: &ObjectMeta,
    log_namespace: LogNamespace,
) {
    if let Some(labels) = &metadata.labels {
        let legacy_key_prefix = fields_spec.node_labels.path.as_ref().map(|k| &k.path);

        let labels = match &fields_spec.node_labels_include {
            Some(keys) => Either::Left(keys.iter().filter_map(|key| labels.get_key_value(key))),
            None => Either::Right(labels.iter()),
        };
        for (key, value) in labels {
            let key_path = path!(key);
            let legacy_key = legacy_key_prefix
                .map(|k| k.concat(key_path))
                .map(LegacyKey::Overwrite);

            log_namespace.insert_source_metadata(
                Config::NAME,
                log,
                legacy_key,
                path!("node_labels", key),
                value.to_owned(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;
    use vector_lib::lookup::{event_path, lookup_v2::parse_target_path, metadata_path};

    use super::*;

    #[allow(
        clippy::too_many_lines,
        reason = "Splitting this fixture is deferred to keep the lint cleanup mechanical. https://github.com/vectordotdev/vector/issues/23659"
    )]
    #[test]
    fn test_annotate_from_metadata() {
        let cases = vec![
            (
                FieldsSpec::default(),
                ObjectMeta::default(),
                LogEvent::default(),
                LogNamespace::Legacy,
            ),
            (
                FieldsSpec::default(),
                ObjectMeta {
                    name: Some("sandbox0-name".to_owned()),
                    uid: Some("sandbox0-uid".to_owned()),
                    labels: Some(
                        vec![
                            ("sandbox0-label0".to_owned(), "val0".to_owned()),
                            ("sandbox0-label1".to_owned(), "val1".to_owned()),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    ..ObjectMeta::default()
                },
                {
                    let mut log = LogEvent::default();
                    log.insert(
                        metadata_path!("kubernetes_logs", "node_labels", "sandbox0-label0"),
                        "val0",
                    );
                    log.insert(
                        metadata_path!("kubernetes_logs", "node_labels", "sandbox0-label1"),
                        "val1",
                    );
                    log
                },
                LogNamespace::Vector,
            ),
            (
                FieldsSpec::default(),
                ObjectMeta {
                    name: Some("sandbox0-name".to_owned()),
                    uid: Some("sandbox0-uid".to_owned()),
                    labels: Some(
                        vec![
                            ("sandbox0-label0".to_owned(), "val0".to_owned()),
                            ("sandbox0-label1".to_owned(), "val1".to_owned()),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    ..ObjectMeta::default()
                },
                {
                    let mut log = LogEvent::default();
                    log.insert(
                        event_path!("kubernetes", "node_labels", "sandbox0-label0"),
                        "val0",
                    );
                    log.insert(
                        event_path!("kubernetes", "node_labels", "sandbox0-label1"),
                        "val1",
                    );
                    log
                },
                LogNamespace::Legacy,
            ),
            (
                FieldsSpec {
                    node_labels: parse_target_path("node_labels").unwrap().into(),
                    ..FieldsSpec::default()
                },
                ObjectMeta {
                    name: Some("sandbox0-name".to_owned()),
                    uid: Some("sandbox0-uid".to_owned()),
                    labels: Some(
                        vec![
                            ("sandbox0-label0".to_owned(), "val0".to_owned()),
                            ("sandbox0-label1".to_owned(), "val1".to_owned()),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    ..ObjectMeta::default()
                },
                {
                    let mut log = LogEvent::default();
                    log.insert(event_path!("node_labels", "sandbox0-label0"), "val0");
                    log.insert(event_path!("node_labels", "sandbox0-label1"), "val1");
                    log
                },
                LogNamespace::Legacy,
            ),
            // Suppressing the legacy key must not suppress the Vector namespace
            // metadata, which is always stored under `%kubernetes_logs`.
            (
                FieldsSpec {
                    node_labels: OptionalTargetPath::none(),
                    ..FieldsSpec::default()
                },
                ObjectMeta {
                    name: Some("sandbox0-name".to_owned()),
                    uid: Some("sandbox0-uid".to_owned()),
                    labels: Some(
                        vec![("sandbox0-label0".to_owned(), "val0".to_owned())]
                            .into_iter()
                            .collect(),
                    ),
                    ..ObjectMeta::default()
                },
                {
                    let mut log = LogEvent::default();
                    log.insert(
                        metadata_path!("kubernetes_logs", "node_labels", "sandbox0-label0"),
                        "val0",
                    );
                    log
                },
                LogNamespace::Vector,
            ),
            // With the legacy namespace, an empty path does suppress the key.
            (
                FieldsSpec {
                    node_labels: OptionalTargetPath::none(),
                    ..FieldsSpec::default()
                },
                ObjectMeta {
                    name: Some("sandbox0-name".to_owned()),
                    uid: Some("sandbox0-uid".to_owned()),
                    labels: Some(
                        vec![("sandbox0-label0".to_owned(), "val0".to_owned())]
                            .into_iter()
                            .collect(),
                    ),
                    ..ObjectMeta::default()
                },
                LogEvent::default(),
                LogNamespace::Legacy,
            ),
        ];

        for (fields_spec, metadata, expected, log_namespace) in cases {
            let mut log = LogEvent::default();
            annotate_from_metadata(&mut log, &fields_spec, &metadata, log_namespace);
            assert_eq!(log, expected);
        }
    }

    #[test]
    fn test_include_node_labels() {
        let zone = "topology.kubernetes.io/zone";
        let metadata = ObjectMeta {
            labels: Some(
                [
                    (zone.to_owned(), "mow".to_owned()),
                    ("kubernetes.io/hostname".to_owned(), "node-1".to_owned()),
                    (
                        "cpu-feature.node.kubevirt.io/aes".to_owned(),
                        "true".to_owned(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            ..ObjectMeta::default()
        };

        for log_namespace in [LogNamespace::Legacy, LogNamespace::Vector] {
            let fields_spec: FieldsSpec = serde_yaml::from_str(
                "node_labels: custom.labels\nnode_labels_include: [topology.kubernetes.io/zone, missing]",
            )
            .unwrap();
            let mut log = LogEvent::default();
            annotate_from_metadata(&mut log, &fields_spec, &metadata, log_namespace);

            let mut expected = LogEvent::default();
            match log_namespace {
                LogNamespace::Legacy => {
                    expected.insert(event_path!("custom", "labels", zone), "mow");
                }
                LogNamespace::Vector => {
                    expected.insert(
                        metadata_path!("kubernetes_logs", "node_labels", zone),
                        "mow",
                    );
                }
            }
            assert_eq!(log, expected);

            let fields_spec = FieldsSpec {
                node_labels_include: Some(BTreeSet::new()),
                ..FieldsSpec::default()
            };
            let mut log = LogEvent::default();
            annotate_from_metadata(&mut log, &fields_spec, &metadata, log_namespace);
            assert_eq!(log, LogEvent::default());

            let fields_spec = FieldsSpec {
                node_labels: OptionalTargetPath::none(),
                node_labels_include: Some([zone.to_owned()].into()),
            };
            let mut log = LogEvent::default();
            annotate_from_metadata(&mut log, &fields_spec, &metadata, log_namespace);
            let mut expected = LogEvent::default();
            if log_namespace == LogNamespace::Vector {
                expected.insert(
                    metadata_path!("kubernetes_logs", "node_labels", zone),
                    "mow",
                );
            }
            assert_eq!(log, expected);
        }
    }
}
