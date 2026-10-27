---
type: Service
title: Time server # trailing comment
description: "GPS stratum 1: the network's clock"
resource: ntp://mosaic.internal
aliases: [mosaic, 'gps clock', "the \"clock\""]
tags:
  - time
  - gps
status: stable
generated: { by: claude-code/claude-opus-5-5, at: 2026-10-26T23:01:11Z }
verified:
  - { by: "human:pgrace", at: 2026-10-25T10:00:00Z }
  - by: claude-code/claude-opus-5-5
    at: 2026-10-26T01:02:03Z
reviewed:
  by: claude-code/claude-opus-5-5
  at: 2026-10-26T02:00:00Z
  outcome: verified
  note: "Checked chrony: offset < 1 ms"
open_questions:
  - "Is the antenna on the roof?"
sources:
  - id: chrony
    resource: chronyc tracking
    title: Live check
  - id: empty-title
    resource: x
    title:
custom_null: ~
custom_block: |
  line one
    indented

  last
custom_folded: >
  folded
  text
custom_flow_multi: [a,
  b, c]
escapes: "tab\thereé\x41 \\ end"
single: 'it''s'
url: https://example.com/a:b
---

# Overview

The [clock](/infra/time-server.md#overview) feeds [hosts](/infra/hosts/).
See [the root](../index.md) and ![a diagram](d.png).

## Overview

```
# not a heading
```

### Plan: CLI first, MCP later

<a name="custom-anchor"></a>

Footnoted claim.[^chrony]

[^chrony]: chronyc tracking
    continued here
