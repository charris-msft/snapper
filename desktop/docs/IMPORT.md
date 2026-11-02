# Native desktop source provenance

The initial `desktop` tree is the tracked SteadyCapture source snapshot at
`331c309e1e171f70e5c02de6d71a084c67ca8304`, authored by Chris Harris with
Copilot App attribution. Its parent is
`e41c47399bed621b66c63c6b585d99ac0477d87e`.

The import used Git's immutable archive, not a working-directory copy.
Toolchains, recordings, private evidence, caches, binaries and reference
screenshots were not imported. The original MIT license and contributor
copyright notice remain in `desktop/LICENSE`.

The frozen foundation includes the supervised native Windows capture and
media pipeline, identity-bound recovery, generation-safe image publication,
bounded thumbnails and the main-canvas renderer smoke oracle. Importing it
does not establish native-window or personal-device acceptance of a new build.

## Integration boundary

Snapper has two separate entry points. The existing Go CLI remains in `go`
with its original arguments, JSON output and exit codes. The Rust application
is built independently under `desktop`; it does not replace the default CLI
route, call synchronous `PrintWindow` on its UI thread, or require the Go
binary for desktop capture.

The consolidation is local-only. The existing repository is public, but
local integration and local packaging do not authorize a push, release,
external upload or replacement of an installed CLI.
