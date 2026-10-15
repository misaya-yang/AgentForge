# Third-party notice

This plugin is a security-focused derivative of selected software engineering
agents published in the GitHub Awesome Copilot repository.

- Upstream repository: https://github.com/github/awesome-copilot
- Pinned commit: 0a6e37e4e242c944380228fa29dbd14e64ac1b63
- Upstream plugin manifest: plugins/software-engineering-team/plugin.json
- Upstream agent definitions:
  - agents/se-security-reviewer.agent.md
  - agents/se-system-architecture-reviewer.agent.md
  - agents/se-technical-writer.agent.md
- Upstream license: MIT

Local modifications:

- converted the definitions to the com.misaya.ai-gateway agents extension;
- narrowed every role to the read-only explore base type;
- replaced upstream tool names with platform-owned category constraints;
- added fixed turn, tool-call, token, and wall-clock budgets;
- removed state-changing workspace, repository, issue-tracker, and deployment
  behavior;
- added prompt-injection, secret-handling, evidence, and scope boundaries;
- shortened generic examples in favor of repository-grounded output contracts.

No upstream executable, hook, command bundle, MCP configuration, or network
installer is included. The bundled LICENSE preserves the upstream notice.
