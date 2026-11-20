# Security policy

## Reporting a vulnerability

Report it privately through GitHub's
[security advisory form](https://github.com/hupe1980/eebus/security/advisories/new), not
in a public issue. Say which version, which feature flags, and what a peer on the LAN can
do with it.

You get an answer within five working days. A confirmed vulnerability is fixed in a patch
release, published as a GitHub security advisory and to the RustSec database, and named in
the changelog.

## Supported versions

The latest minor release. Before 1.0 a fix is not backported.

## Scope

In scope: anything a SHIP or SPINE peer, an mDNS announcement or a Pairing Service request
can make this crate do — crash it, grow it without bound, trust the wrong peer, or apply a
value nobody sent.

Out of scope, by design: a peer holding the node's private key, a peer a user approved,
and the policy a paired Energy Guard is subject to beyond its binding. The
[security model](https://hupe1980.github.io/eebus/docs/security/) says why.

A manufacturer with Cyber Resilience Act reporting duties: this policy and the RustSec
advisory are how you hear about a vulnerability here.
