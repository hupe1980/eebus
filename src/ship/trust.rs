//! How much a peer is trusted, and what that permits (SHIP §12.3.2).
//!
//! SHIP's trust is not a boolean. §12.3.2 defines three independent categories — *user
//! trust*, *PKI trust* and *second factor trust* — each a number, and Table 10 maps every
//! way a public key can come to be accepted onto values in them. A stack that models trust
//! as yes-or-no cannot express the rules the specification then states about those numbers,
//! and there are three of them, each a SHALL:
//!
//! * **Below user trust 8, do not talk at all.** "A 'user trust' of '8' is the minimal
//!   'user trust' that is required for general SHIP communication. This means if the 'user
//!   trust' is less than '8', the communication SHALL be aborted."
//! * **Below user trust 32, do not send the PIN.** §12.5: "the SHIP node PIN SHALL NOT be
//!   transmitted if the public key of the corresponding communication partner has a user
//!   trust level that is less than '32'." A PIN is an authentication secret, and a peer
//!   admitted by auto-accept alone is at 8.
//! * **Commissioning over SHIP needs 32 somewhere.** "a trust level of '32' or higher MUST
//!   be achieved in the 'user trust level' or 'second factor trust level' category".
//!
//! And §12.3.2's own closing paragraph is why this is public rather than internal: "A layer
//! above SHIP can use the trust level to control access to certain functionality. The trust
//! level requirements MAY differ depending on the feature […] Some privacy relevant use
//! cases might require a high 'user trust'". A §14a box deciding whether the thing on the
//! other end may set a grid limit is exactly that layer.
//!
//! ```
//! use eebus::ship::TrustLevel;
//!
//! // A peer a user compared forty hex digits for.
//! let verified = TrustLevel::USER_VERIFIED;
//! assert!(verified.permits_communication());
//! assert!(verified.permits_pin_transmission());
//!
//! // One let in by auto-accept alone — which SHIP IG §2.3 forbids anyway — may talk,
//! // and must not be sent a PIN.
//! let weak = TrustLevel::AUTO_ACCEPT;
//! assert!(weak.permits_communication());
//! assert!(!weak.permits_pin_transmission());
//!
//! // §12.3.2: within a category, the strongest mechanism is the one that counts.
//! assert_eq!(weak.merged(verified), verified);
//! ```

/// The least *user trust* SHIP will exchange any data at (§12.3.2).
///
/// "if the 'user trust' is less than '8', the communication SHALL be aborted".
pub const MIN_USER_TRUST: u16 = 8;

/// The trust — in either the user or the second-factor category — that commissioning over
/// SHIP requires (§12.3.2), and that §12.5 requires before a PIN may be sent.
pub const COMMISSIONING_TRUST: u16 = 32;

/// What a peer is trusted at, by category (SHIP §12.3.2, Table 10).
///
/// The three categories are independent: a peer can be strongly PKI-trusted and unknown to
/// the user, or the reverse. Build one from the mechanism that admitted the peer — the
/// associated constants are Table 10 — and combine several with [`merged`](Self::merged).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustLevel {
    /// How far a *person* has vouched for this key.
    pub user: u16,
    /// How far a public key infrastructure has. "PKI certificates are not mandatory, hence
    /// general communication SHALL also be possible without the use of a trusted PKI and a
    /// 'PKI trust level' of '0'."
    pub pki: u16,
    /// How far a second factor has. The PIN is currently the only one SHIP defines, and
    /// §12.5 caps it at 16 for every peer but the first (see
    /// [`SECOND_FACTOR_PIN_FIRST`](Self::SECOND_FACTOR_PIN_FIRST)).
    pub second_factor: u16,
}

impl TrustLevel {
    /// Nothing vouches for this key. Below [`MIN_USER_TRUST`], so no data may be exchanged.
    pub const UNTRUSTED: Self = Self::user(0);

    /// `auto accept` — user trust 8.
    ///
    /// The lowest level SHIP will talk at, and one this crate never assigns itself: the
    /// SHIP implementation guide §2.3 forbids an auto-accept mode outright, and
    /// [`TrustStore`](crate::runtime::TrustStore) has none. It is here because a peer may
    /// have been admitted that way by an application that built its own store, and because
    /// the PIN rule of §12.5 exists precisely to say what such a peer may not be told.
    pub const AUTO_ACCEPT: Self = Self::user(8);

    /// `user verified` — user trust 32.
    ///
    /// A person compared the SKI against the label on the device and said yes. This is what
    /// [`TrustStore::trust`](crate::runtime::TrustStore::trust) means, and what
    /// [`HubEvent::TrustRequested`](crate::runtime::HubEvent::TrustRequested) is answered
    /// with.
    pub const USER_VERIFIED: Self = Self::user(32);

    /// `user input` — user trust 64. A person entered the key rather than confirming it.
    pub const USER_INPUT: Self = Self::user(64);

    /// `auto accept + PIN` — user trust 32 with second factor 16.
    ///
    /// The QR-code flow of §12.5: a node auto-accepts and then waits for a PIN only its
    /// owner can have. The second factor is 16 rather than 32 unless this is the *first*
    /// peer to present a valid PIN since factory default — see
    /// [`SECOND_FACTOR_PIN_FIRST`](Self::SECOND_FACTOR_PIN_FIRST).
    pub const AUTO_ACCEPT_WITH_PIN: Self = Self {
        user: 32,
        pki: 0,
        second_factor: 16,
    };

    /// The second factor the **first** peer to send a valid PIN after a factory default may
    /// be awarded (§12.5): 32, which is what lets it act as a SHIP commissioning tool.
    ///
    /// "In SHIP, it is not possible that two SHIP nodes may gain a second factor trust of
    /// '32' with the SHIP node PIN. Any SHIP node that sends the PIN afterwards SHALL only
    /// get a second factor trust of '16'."
    /// [`TrustStore::award_pin_trust`](crate::runtime::TrustStore::award_pin_trust) is that
    /// rule, because only a store that survives a reconnection can apply "the first".
    pub const SECOND_FACTOR_PIN_FIRST: u16 = 32;

    /// The second factor every peer after the first is capped at (§12.5).
    pub const SECOND_FACTOR_PIN_LATER: u16 = 16;

    /// `SHIP commissioning` variant (a): user trust 8 with second factor 32.
    pub const SHIP_COMMISSIONED_BY_PIN: Self = Self {
        user: 8,
        pki: 0,
        second_factor: 32,
    };

    /// `SHIP commissioning` variant (b): user trust 32.
    pub const SHIP_COMMISSIONED_BY_USER: Self = Self::user(32);

    /// A level in the user category alone.
    pub const fn user(user: u16) -> Self {
        Self {
            user,
            pki: 0,
            second_factor: 0,
        }
    }

    /// `commissioned` — user trust 32 to 96, "depending on trustworthiness of commissioning
    /// tool" (Table 10).
    ///
    /// The range is the specification's and the choice within it is the device's, so this
    /// clamps rather than refusing: a commissioning tool that reports something outside it
    /// has misread the table, and a *lower* trust than the mechanism warrants is the safe
    /// way to be wrong.
    pub const fn commissioned(user: u16) -> Self {
        let clamped = if user < 32 {
            32
        } else if user > 96 {
            96
        } else {
            user
        };
        Self::user(clamped)
    }

    /// A PKI trust level, "0-65535 depending on SHIP node policy / trust in certain PKI".
    #[must_use]
    pub const fn with_pki(mut self, pki: u16) -> Self {
        self.pki = pki;
        self
    }

    /// A second-factor trust level.
    #[must_use]
    pub const fn with_second_factor(mut self, second_factor: u16) -> Self {
        self.second_factor = second_factor;
        self
    }

    /// The strongest of each category, taken separately (§12.3.2).
    ///
    /// "If multiple mechanisms are used from the same category, only the mechanism which
    /// offers the highest trust level in this category SHALL be accounted for. E.g. if a
    /// SHIP node has verified a public key with 'auto accept' and 'user verify', only 'user
    /// verify' is accounted for and therefore the 'user trust' value is '32'."
    ///
    /// Categories do not add: a second factor of 32 does not make a user trust of 8 into 40.
    #[must_use]
    pub fn merged(self, other: Self) -> Self {
        Self {
            user: self.user.max(other.user),
            pki: self.pki.max(other.pki),
            second_factor: self.second_factor.max(other.second_factor),
        }
    }

    /// Whether SHIP will exchange data with this peer at all (§12.3.2).
    ///
    /// User trust of at least [`MIN_USER_TRUST`]. PKI trust does not substitute: §12.3.2's
    /// note is explicit that "optional PKI is no verification mode, as it does not offer
    /// the necessary user trust".
    pub fn permits_communication(self) -> bool {
        self.user >= MIN_USER_TRUST
    }

    /// Whether this peer may be sent this node's PIN (§12.5).
    ///
    /// "The PIN is an authentication secret that must be kept confidential and SHALL only
    /// be shared with authenticated and authorized communication partners. Therefore, the
    /// SHIP node PIN SHALL NOT be transmitted if the public key of the corresponding
    /// communication partner has a user trust level that is less than '32'."
    pub fn permits_pin_transmission(self) -> bool {
        self.user >= COMMISSIONING_TRUST
    }

    /// Whether this peer may commission over SHIP (§12.3.2).
    ///
    /// "a trust level of '32' or higher MUST be achieved in the 'user trust level' **or**
    /// 'second factor trust level' category" — either alone is enough, which is what lets
    /// a phone that has proved it holds the PIN act as a commissioning tool for a node
    /// whose SKI nobody has read out.
    pub fn permits_ship_commissioning(self) -> bool {
        self.user >= COMMISSIONING_TRUST || self.second_factor >= COMMISSIONING_TRUST
    }
}

impl core::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "user {}, pki {}, second factor {}",
            self.user, self.pki, self.second_factor
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 10, transcribed and checked against the three rules that read it.
    #[test]
    fn table_10_permits_what_the_specification_says_it_does() {
        for (level, communication, pin, commissioning) in [
            (TrustLevel::UNTRUSTED, false, false, false),
            (TrustLevel::AUTO_ACCEPT, true, false, false),
            (TrustLevel::USER_VERIFIED, true, true, true),
            (TrustLevel::USER_INPUT, true, true, true),
            (TrustLevel::AUTO_ACCEPT_WITH_PIN, true, true, true),
            // Variant (a): user trust 8 is below the PIN threshold, and the second factor
            // alone is what carries commissioning.
            (TrustLevel::SHIP_COMMISSIONED_BY_PIN, true, false, true),
            (TrustLevel::SHIP_COMMISSIONED_BY_USER, true, true, true),
        ] {
            assert_eq!(level.permits_communication(), communication, "{level}");
            assert_eq!(level.permits_pin_transmission(), pin, "{level}");
            assert_eq!(level.permits_ship_commissioning(), commissioning, "{level}");
        }
    }

    /// "optional PKI is no verification mode, as it does not offer the necessary user
    /// trust" — so no amount of it opens a connection.
    #[test]
    fn pki_trust_alone_never_permits_communication() {
        let pki_only = TrustLevel::UNTRUSTED.with_pki(u16::MAX);
        assert!(!pki_only.permits_communication());
        assert!(!pki_only.permits_pin_transmission());
        assert!(!pki_only.permits_ship_commissioning());
    }

    /// §12.3.2's own worked example: auto accept and user verify give 32, not 40.
    #[test]
    fn categories_take_the_strongest_rather_than_adding_up() {
        let both = TrustLevel::AUTO_ACCEPT.merged(TrustLevel::USER_VERIFIED);
        assert_eq!(both, TrustLevel::USER_VERIFIED);
        assert_eq!(both.user, 32);

        // And across categories nothing accumulates: a second factor of 32 leaves a user
        // trust of 8 where it was, which is why variant (a) may commission and may not be
        // sent a PIN.
        let a = TrustLevel::SHIP_COMMISSIONED_BY_PIN;
        assert_eq!(a.user, 8);
        assert!(a.permits_ship_commissioning() && !a.permits_pin_transmission());
    }

    /// Table 10 gives `commissioned` a range, and the choice within it to the device.
    #[test]
    fn a_commissioned_level_stays_inside_the_range_the_table_gives() {
        assert_eq!(TrustLevel::commissioned(64).user, 64);
        assert_eq!(
            TrustLevel::commissioned(0).user,
            32,
            "clamped up to the floor"
        );
        assert_eq!(
            TrustLevel::commissioned(1_000).user,
            96,
            "and down to the ceiling"
        );
    }
}
