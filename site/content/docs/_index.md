+++
title = "Documentation"
description = "Guides to the eebus crate: the EEBUS standard, the SHIP transport, the SPINE model, and the grid use cases behind §14a EnWG — with runnable Rust."
sort_by = "weight"
template = "section.html"
page_template = "page.html"
+++

This documentation explains both halves of the problem: **what EEBUS actually requires**,
and **how this crate expresses it**. Neither is much use alone — the standard is four
specifications and a stack of implementation guides, and a Rust API is meaningless without
knowing which sentence of which document it is honouring.

## Where to start

If you have never met EEBUS, read [What EEBUS is](@/docs/introduction.md) first: the
players, the layers, and why a German grid regulation made this a certifiable protocol
rather than a hobby one.

If you know the standard and want the code, go straight to
[Getting started](@/docs/getting-started.md) and then the use case you need:
[LPC and LPP](@/docs/limitation.md) for limiting power,
[MPC and MGCP](@/docs/monitoring.md) for measuring it,
[E-mobility](@/docs/e-mobility.md) for wallboxes and cars, and
[inverters, PV and batteries](@/docs/storage.md) for the generation side.

If you are evaluating the crate, [Architecture](@/docs/architecture.md),
[Conformance](@/docs/conformance.md) and [Certification](@/docs/certification.md) are the
pages that answer "is this real", and
[Which functions can be exchanged in part](@/docs/functions.md) answers the one SPINE
question that has no short answer — it is the generated table itself, all 142 rows.

## Conventions

Citations name the document and the section: **SHIP §12.2.3** is the SHIP Technical
Specification, **SPINE IG §3.2.1** the SPINE implementation guide, **[LPC-002]** a numbered
requirement from a use-case implementation guide. Every one of them appears in the source
too, next to the code that satisfies it.

The API reference — every type, every function — lives on
[docs.rs](https://docs.rs/eebus). These pages are the map; that is the territory.
