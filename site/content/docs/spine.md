+++
title = "SPINE: the model and the protocol"
description = "The SPINE device model, addressing, NodeManagement discovery, bindings and subscriptions, acknowledgements, and Restricted Function Exchange with its merge rule."
weight = 60
[extra]
group = "Protocol"
+++

SPINE defines what a device *says it is* and how peers read, write and subscribe to it.

## The model

```text
Device        d:_i:46925_HeatPump-1
 └─ Entity    [0]  DeviceInformation   ← always present
     └─ Feature 1  NodeManagement      ← the entry point for every peer
 └─ Entity    [1]  HeatPumpAppliance
     ├─ Feature 1  LoadControl         ← the limit lives here
     └─ Feature 2  DeviceDiagnosis     ← the heartbeat lives here
```

An entity path is a list, so entities nest. A **feature address** is the triple
`device / entity / feature`, and it is what every message carries as source and destination.

A feature declares a **role** — server (holds the data) or client (asks for it) — and, per
function, which **operations** are possible: read, write, and whether partial forms are
supported. `possibleOperations` is a promise, and the crate treats it as one: it announces
a partial read only for functions it can genuinely restrict.

## Discovery

Every conversation begins at NodeManagement on entity 0.

* **Detailed discovery** (§7.1) — "what are you?" Returns the whole tree: entities,
  features, roles, functions, operations.
* **Use-case discovery** (§7.3) — "what do you do?" Returns
  `nodeManagementUseCaseData`: the use cases played, the actor played in each, and which
  numbered scenarios are supported.

An application that skips the second and guesses from feature types will eventually meet a
device that has a `LoadControl` feature for an unrelated reason.

## Bindings and subscriptions

Two distinct grants, easily confused:

* A **binding** grants the right to *write* to a feature. Without one, a write is refused.
* A **subscription** asks to be *notified* when data changes.

LPC requires both, in that order, and requires the Energy Guard to have subscribed to the
heartbeat *before* it writes a limit. A single-writer policy applies: a feature accepts a
binding from one client, and the group lock the LPC implementation guide §3.5 describes is
enforced here rather than left to the application.

Unchanged data is not notified (SPINE IG §2.4). That is what makes a dozen subscriptions
affordable on a small controller.

Both tables are readable — `nodeManagementBindingData` and `nodeManagementSubscriptionData`
(§7.3.2, §7.4.2) — and both are answered from the relations actually held.

## Deciding a write

A feature may hand its writes to the application instead of storing them, which is what LPC
needs: the acknowledgement *is* the record that the limit was applied, so only the appliance
can produce it. The engine reports `SpineEvent::WriteRequested` and waits. It carries two
payloads:

* `data` — what arrived, exactly. It says *which* entries the write addresses, and it is
  the record of what was asked for.
* `resolved` — what the function would hold if the write were accepted: `data` merged into
  what is stored.

Decide on `resolved`. A partial write carries only what changed, so an absent
`isLimitActive` means "unchanged", not "deactivate".

A write is refused with `errorNumber` 7 when an entry arrives without its primary and sub
identifiers, which use-case IG §3.1 requires in every message: such an entry names nothing
stored, so it can neither update nor delete.

## Acknowledgements and errors

A message may request an acknowledgement, and whether one is owed depends on the classifier,
the outcome and the request together (§5.2.5.1). A successful `read` is answered by the
reply itself, never by a separate result; a failed one still owes an error. A `result` is
never answered by a result — that way lies a loop.

```rust
owes_ack(CmdClassifier::Read,  true,  ErrorNumber::None);                 // false
owes_ack(CmdClassifier::Read,  true,  ErrorNumber::CommandNotSupported);  // true
owes_ack(CmdClassifier::Write, true,  ErrorNumber::None);                 // true
owes_ack(CmdClassifier::Write, false, ErrorNumber::None);                 // false
```

Message counters are per-source and monotonic; a datagram addressed to a device that is not
this one is answered `DestinationUnreachable` rather than ignored.

`specificationVersion` is checked, and a malformed one is refused —
`TC_SPINE_COMP_006` calls that the recommended behaviour — while tolerating the one
deviation it permits, a leading `v`.

## Restricted Function Exchange

RFE is how SPINE reads and writes *part* of a function: a **selector** says which entries,
an **elements** filter says which fields of them.

```rust
data.restrict(selectors, elements)?;                  // what a partial read answers with
rfe::apply_partial(&mut stored, update);              // merge by identifier, element by element
rfe::delete_elements(&mut stored, |e| …, &elements);  // LPC §3.4.1.4's delete-and-write
```

**The merge rule is the part to get right.** An omitted element means *unchanged*, all the
way down the tree. A `scaledNumber` arriving with a bare `number` must keep its stored
`scale`, or a 4.2 kW limit becomes 42 MW.

Nothing in the XML Schemas links a data type to its selectors and elements filters — the
link is a naming convention — so the generator resolves it into a table covering 141
functions, and merge, delete and partial read are written once against that table.

A filtered request for a function the table does not cover is refused with `errorNumber` 8
rather than answered approximately. Announcing what you cannot do is worse than announcing
less.
