+++
title = "Documentation"
description = "Guides to the eebus crate: the EEBUS standard, the SHIP transport, the SPINE model, and the grid use cases behind §14a EnWG — with runnable Rust."
sort_by = "weight"
template = "section.html"
page_template = "page.html"
+++

This documentation explains **what EEBUS requires** and **how this crate expresses it**.

## Where to start

New to EEBUS: read [What EEBUS is](@/docs/introduction.md) first — the actors, the
layers, and the regulation behind them.

Know the standard: start at [Getting started](@/docs/getting-started.md), then the use
case you need:
[LPC and LPP](@/docs/limitation.md) for limiting power,
[MPC and MGCP](@/docs/monitoring.md) for measuring it,
[E-mobility](@/docs/e-mobility.md) for wallboxes and cars,
[inverters, PV and batteries](@/docs/storage.md) for the generation side, and
[heat pumps](@/docs/hot-water.md) for the hot water, the building's temperature and
the compressor that heats it.

Evaluating the crate: [Architecture](@/docs/architecture.md),
[Conformance](@/docs/conformance.md) and [Certification](@/docs/certification.md).
[Which functions can be exchanged in part](@/docs/functions.md) is the generated table of
all 142 functions.

## Conventions

Citations name the document and the section: **SHIP §12.2.3** is the SHIP Technical
Specification, **SPINE IG §3.2.1** the SPINE implementation guide, **[LPC-002]** a numbered
requirement from a use-case implementation guide. The source cites them too, next to the
code that satisfies them.

The API reference is on [docs.rs](https://docs.rs/eebus).
