use crate::Usage;

/// Prices are USD per million tokens.
pub struct Model {
    pub id: &'static str,
    pub context: u32,
    pub price_in: f64,
    pub price_cached: f64,
    pub price_out: f64,
}

pub const DEFAULT_CONTEXT: u32 = 128_000;

const MODELS: &[Model] = &[
    Model {
        id: "deepseek-chat",
        context: 128_000,
        price_in: 0.28,
        price_cached: 0.028,
        price_out: 0.42,
    },
    Model {
        id: "deepseek-reasoner",
        context: 128_000,
        price_in: 0.28,
        price_cached: 0.028,
        price_out: 0.42,
    },
    Model {
        id: "gpt-5",
        context: 400_000,
        price_in: 1.25,
        price_cached: 0.125,
        price_out: 10.0,
    },
    Model {
        id: "gpt-5-mini",
        context: 400_000,
        price_in: 0.25,
        price_cached: 0.025,
        price_out: 2.0,
    },
];

pub fn find(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

impl Model {
    pub fn cost(&self, u: &Usage) -> f64 {
        let fresh = u.input.saturating_sub(u.cached);
        (fresh as f64 * self.price_in
            + u.cached as f64 * self.price_cached
            + u.output as f64 * self.price_out)
            / 1e6
    }
}
