---
title: dekit runner pause
cli: dekit runner pause
related: [cli/runner, cli/runner/stop, cli/runner/start]
---

Saves every task and its screen, then stops the runner. The next start of
that runner registers the saved tasks again: config tasks around their
saved screens, and tasks that were added at runtime from their saved
definitions. Pinned tasks start again; the others show their last screen
until they are started. Not available on Windows.

:::usage
