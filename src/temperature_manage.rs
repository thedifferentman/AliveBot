use crate::CONFIG;
use std::time::Instant;

#[derive(Debug)]
pub struct TemperatureDecay {
    decay: f64,
    updated_at: Instant,
}

impl TemperatureDecay {
    pub fn new() -> Self {
        Self {
            decay: 0.0,
            updated_at: Instant::now(),
        }
    }

    pub fn get_decay(&self) -> f64 {
        let seconds = Instant::now()
            .saturating_duration_since(self.updated_at)
            .as_secs_f64();

        (self.decay - seconds * 0.01).max(0.0)
    }

    pub fn get_temp(&self) -> f64 {
        (CONFIG.get().unwrap().temperature - self.get_decay()).max(0.0)
    }

    pub fn skip_probability(&self) -> f64 {
        (self.get_decay() / CONFIG.get().unwrap().temperature / 2.0).min(1.0)
    }

    pub fn increase(&mut self) {
        let seconds = Instant::now()
            .saturating_duration_since(self.updated_at)
            .as_secs_f64();
        self.decay = (self.decay - seconds * 0.01).max(0.0) + 0.2;
        self.updated_at = Instant::now();
    }
}
