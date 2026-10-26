The `kubernetes_logs` source supports `label_filters.{pod,namespace,node}.{includes,excludes}`
to select label keys with glob patterns before storing objects in its caches and enriching events.
Filters default to all labels, and exclusions take precedence over inclusions.

authors: sakateka
