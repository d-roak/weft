//! x402 payments over weft.
//!
//! [x402](https://x402.org) revives HTTP status **402 Payment Required** as a
//! machine-payable handshake: a server answers an unpaid request with the price
//! and where to pay; the client pays, attaches proof, and retries. weft carries
//! that same handshake over its P2P channel so agents can charge each other for:
//!
//! - **Relaying** — a node with good connectivity sells relay/forwarding.
//! - **API / service discovery** — a discovered service is gated behind a price.
//! - **Any agent capability** — inference, data, compute, tool calls.
//!
//! The flow is three [`AgentMessage`](crate::AgentMessage)s:
//! 1. client → server: `"invoke"` (no payment)
//! 2. server → client: `"x402/payment-required"` carrying [`PaymentRequired`]
//! 3. client → server: `"invoke"` again, `body.payment` = [`PaymentPayload`]
//!
//! Settlement itself (submitting an on-chain transfer, checking a facilitator)
//! is deliberately out of scope here — [`verify_payment`] is the single seam
//! where you plug a real facilitator in.

use serde::{Deserialize, Serialize};

pub const KIND_PAYMENT_REQUIRED: &str = "x402/payment-required";

/// What a resource costs and where to pay. Mirrors an x402 `accepts` entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceTag {
    /// Amount in the asset's smallest unit (e.g. USDC has 6 decimals).
    pub amount: u64,
    /// Asset symbol or contract identifier, e.g. `"USDC"`.
    pub asset: String,
    /// Network the payment settles on, e.g. `"base"` / `"base-sepolia"`.
    pub network: String,
    /// Address that should receive payment.
    pub pay_to: String,
    /// Payment scheme, e.g. `"exact"` (the x402 default).
    #[serde(default = "default_scheme")]
    pub scheme: String,
}

fn default_scheme() -> String {
    "exact".to_string()
}

impl PriceTag {
    pub fn new(amount: u64, asset: &str, network: &str, pay_to: &str) -> Self {
        Self {
            amount,
            asset: asset.into(),
            network: network.into(),
            pay_to: pay_to.into(),
            scheme: default_scheme(),
        }
    }
}

/// Body of the `x402/payment-required` reply: the price plus a per-request
/// nonce the client must echo back in its payment, binding proof to request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRequired {
    pub price: PriceTag,
    /// Opaque, single-use challenge tying a payment to this exact request.
    pub nonce: String,
    /// Human-readable name of what's being sold.
    pub resource: String,
}

/// Proof of payment a client attaches on retry. In real x402 this is a signed
/// payment payload / transaction the server can verify or settle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentPayload {
    pub scheme: String,
    pub nonce: String,
    /// Scheme-specific proof: a signed authorization, tx hash, or receipt.
    pub proof: String,
}

/// Verify that `payload` satisfies `required`.
///
/// ponytail: stub — checks scheme + nonce match and proof is non-empty. That is
/// enough to exercise the full handshake in tests and examples. Swap the body
/// for a call to an x402 facilitator (verify/settle) to accept real money.
pub fn verify_payment(required: &PaymentRequired, payload: &PaymentPayload) -> bool {
    payload.scheme == required.price.scheme
        && payload.nonce == required.nonce
        && !payload.proof.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_accepts_matching_payment_and_rejects_bad_ones() {
        let req = PaymentRequired {
            price: PriceTag::new(1000, "USDC", "base-sepolia", "0xabc"),
            nonce: "n-123".into(),
            resource: "relay".into(),
        };
        let good = PaymentPayload {
            scheme: "exact".into(),
            nonce: "n-123".into(),
            proof: "0xsigned".into(),
        };
        assert!(verify_payment(&req, &good));

        // wrong nonce (replay / mismatched request) is rejected
        let replay = PaymentPayload { nonce: "n-999".into(), ..good.clone() };
        assert!(!verify_payment(&req, &replay));

        // empty proof is rejected
        let empty = PaymentPayload { proof: String::new(), ..good };
        assert!(!verify_payment(&req, &empty));
    }
}
