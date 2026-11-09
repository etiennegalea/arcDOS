# ArcDOS development

- Read [the architecture overview](docs/architecture/overview.md) and [the roadmap](docs/roadmap.md) before changing service boundaries or beginning a milestone.
- Deliver small increments with tests and keep the workspace buildable. Follow [the development checks](docs/development.md) before reporting completion.
- Keep Protobuf schemas in `proto/` as the source of truth. Regenerate bindings and run Buf compatibility checks when editing a public schema.
- Use Connect RPC for typed application calls and bounded HTTP streaming for file contents. Use opaque identifiers for resources.
- Treat external input as untrusted. Keep filesystem operations confined to the configured root and enforce authorization at the server before exposing file data.
- Keep AI inference independent of file operations. Require user confirmation for AI-proposed changes.
- Use synthetic files or temporary storage in tests. Keep credentials, models, and personal data out of the repository.
- Pin toolchains and dependencies, maintain lockfiles, and verify integrations against upstream documentation. Record architectural changes in [the decision log](docs/architecture/decisions.md).
- Ask for explicit authorization before pushing or deploying publicly.
