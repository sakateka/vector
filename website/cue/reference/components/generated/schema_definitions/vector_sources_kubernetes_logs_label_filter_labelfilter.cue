package metadata

_schemaDefinitions: "vector::sources::kubernetes_logs::label_filter::LabelFilter": object: options: {
	excludes: {
		description: """
			Label key patterns to exclude.

			Exclusions take precedence over inclusions. Omit this option or use an empty list to
			exclude no keys.
			"""
		required: false
		type: array: items: type: string: examples: ["cpu-feature.node.kubevirt.io/*"]
	}
	includes: {
		description: """
			Label key patterns to include.

			Omit this option or set it to `null` to include all keys before exclusions.
			An empty list selects no labels. Overlapping patterns select each key once.
			"""
		required: false
		type: array: items: type: string: examples: ["app.kubernetes.io/*", "topology.kubernetes.io/zone"]
	}
}
