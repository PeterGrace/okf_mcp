# Move to the hypotheosis organization

The repository moved from `PeterGrace/okf_mcp` to `hypotheosis/okf_mcp`.
GitHub redirects the old URLs.

- Images now publish to `ghcr.io/hypotheosis/okf-mcp` and
  `ghcr.io/hypotheosis/okf-maintenance`, starting with the next release.
  Images already published under `ghcr.io/petergrace/` stay there and keep
  working; switch to the new path on your next upgrade.
- The server image carries the label
  `io.modelcontextprotocol.server.name="io.github.hypotheosis/okf-mcp"`,
  which the official MCP Registry requires.
- Updated the image paths in `README.md`, `docs/operations.md`,
  `docs/maintenance-kubernetes.md` and the maintenance image workflow test.
  Historical plans, specs and changelog entries keep the old paths.
