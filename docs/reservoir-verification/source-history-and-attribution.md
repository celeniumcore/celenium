# Source history and upstream attribution

Celenium is an independent fork of [kaspanet/rusty-kaspa](https://github.com/kaspanet/rusty-kaspa). Its GHOSTDAG blockDAG foundation, kHeavyHash proof-of-work implementation, and substantial node and wallet code originate with the Kaspa project. Celenium changes network identity, genesis parameters, emission, vesting, and services around that foundation.

## Maintainer and documentation alias

[Dimas A. Pradana (`dimas405`)](https://github.com/dimas405) maintains the Celenium changes. The owner also uses [`queenstack-dev`](https://github.com/queenstack-dev) for documentation work. These are two aliases of the same maintainer, not evidence of two independent developers.

The published history retains the original author names and dates. Documentation changes attributed to `queenstack-dev` contain real edits; existing commits have not been reassigned to change contribution rankings.

## Upstream credit

The [rusty-kaspa contributor list](https://github.com/kaspanet/rusty-kaspa/graphs/contributors) includes [`michaelsutton`](https://github.com/michaelsutton), [`someone235`](https://github.com/someone235), [`biryukovmaxim`](https://github.com/biryukovmaxim), [`aspect`](https://github.com/aspect), and [`tiram88`](https://github.com/tiram88), among other contributors.

These links acknowledge work in the upstream project. They do not identify Celenium team members, co-authors of Celenium modifications, endorsers, or collaborators with access to this repository. The upstream [ISC copyright notice](../../LICENSE) remains intact.

GitHub's contributor graph is generated from commit metadata. An acknowledgement in this document does not create a contribution to this fork.

## Public backup and history sanitation

The public backup was prepared from an isolated copy of the local Git repositories. A legacy faucet-address helper contained a hardcoded private key. That helper was removed from the published history, and the same value in signer tests was replaced with a conventional test-only fixture.

This removal changes affected commit IDs in the public copy. Author names, timestamps, commit messages, and the work unrelated to that key remain preserved. The original local repositories and their restricted local bundles were not rewritten. The public backup is therefore a sanitized copy, not a byte-for-byte mirror of the original history.

Removing a key from Git does not make the key safe to use again. Never fund addresses derived from disclosed keys. No key material is included in this document.

## Changes during publication

Repository publication and documentation updates do not reset the Celenium chain, change genesis, restart node services, or establish on-chain payment. Verify operational status separately from source availability.

GitHub Actions is disabled for the backup repository. Build and test the source on infrastructure where the workload is authorized before enabling any automation.
