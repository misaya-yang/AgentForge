# Third-party notice

This plugin is a security-focused derivative of the Doublecheck agent and
skill published in the GitHub Awesome Copilot repository.

- Upstream repository: https://github.com/github/awesome-copilot
- Pinned commit: 0a6e37e4e242c944380228fa29dbd14e64ac1b63
- Upstream plugin manifest: plugins/doublecheck/plugin.json
- Upstream agent definition: agents/doublecheck.agent.md
- Upstream skill: skills/doublecheck/SKILL.md
- Upstream license: MIT

Local modifications:

- converted the definition to the com.misaya.ai-gateway agents extension;
- limited the role to one-shot, read-only verification;
- replaced upstream tool names with platform-owned category constraints;
- added fixed turn, tool-call, token, and wall-clock budgets;
- removed persistent-session behavior and all executable integration surfaces;
- added explicit untrusted-content and evidence requirements.

No upstream executable, hook, command bundle, MCP configuration, or network
installer is included. The bundled LICENSE preserves the upstream notice.
