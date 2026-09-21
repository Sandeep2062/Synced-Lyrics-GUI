use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub requests_per_day: u32, // 0 = unlimited
    pub base_interval: Duration,
    pub max_backoff: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 60,
            requests_per_day: 0,
            base_interval: Duration::from_millis(500),
            max_backoff: Duration::from_secs(60),
        }
    }
}

pub struct AdaptiveRateLimiter {
    config: RateLimitConfig,
    state: Mutex<RateLimiterState>,
}

struct RateLimiterState {
    minute_timestamps: Vec<Instant>,
    daily_count: u32,
    daily_reset: Instant,
    current_interval: Duration,
    next_slot: Instant,
}

impl AdaptiveRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        let now = Instant::now();
        Self {
            config: config.clone(),
            state: Mutex::new(RateLimiterState {
                minute_timestamps: Vec::new(),
                daily_count: 0,
                daily_reset: now + Duration::from_secs(86400),
                current_interval: config.base_interval,
                next_slot: now,
            }),
        }
    }

    pub fn wait(&self) {
        loop {
            let mut state = self.state.lock().unwrap();
            let now = Instant::now();

            // prune old timestamps
            state
                .minute_timestamps
                .retain(|&ts| now.duration_since(ts) < Duration::from_secs(60));

            // Check daily limit
            if self.config.requests_per_day > 0 {
                if now >= state.daily_reset {
                    state.daily_count = 0;
                    state.daily_reset = now + Duration::from_secs(86400);
                }
                if state.daily_count >= self.config.requests_per_day {
                    drop(state);
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }

            // Check per-minute capacity
            if state.minute_timestamps.len() >= self.config.requests_per_minute as usize {
                if let Some(&oldest) = state.minute_timestamps.first() {
                    let expiry = oldest + Duration::from_secs(60);
                    if now < expiry {
                        let sleep_dur = expiry.duration_since(now);
                        drop(state);
                        std::thread::sleep(sleep_dur);
                        continue;
                    }
                }
            }

            // Check current_interval
            if now < state.next_slot {
                let sleep_dur = state.next_slot.duration_since(now);
                drop(state);
                std::thread::sleep(sleep_dur);
                continue;
            }

            // All checks passed
            state.minute_timestamps.push(now);
            state.daily_count += 1;
            state.next_slot = now + state.current_interval;
            break;
        }
    }

    pub fn penalize(&self, retry_after: Option<Duration>) {
        let mut state = self.state.lock().unwrap();
        if let Some(dur) = retry_after {
            state.next_slot = Instant::now() + dur;
            state.current_interval = dur.min(self.config.max_backoff);
        } else {
            state.current_interval = (state.current_interval * 2).min(self.config.max_backoff);
            state.next_slot = Instant::now() + state.current_interval;
        }
    }

    pub fn relax(&self) {
        let mut state = self.state.lock().unwrap();
        let current_secs = state.current_interval.as_secs_f64();
        let new_secs = current_secs * 0.9;
        let mut new_interval = Duration::from_secs_f64(new_secs);
        if new_interval < self.config.base_interval {
            new_interval = self.config.base_interval;
        }
        state.current_interval = new_interval;
    }

    pub fn is_available(&self) -> bool {
        if self.config.requests_per_day == 0 {
            return true;
        }
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        if now >= state.daily_reset {
            state.daily_count = 0;
            state.daily_reset = now + Duration::from_secs(86400);
        }
        state.daily_count < self.config.requests_per_day
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relax_and_penalize() {
        let config = RateLimitConfig::default();
        let limiter = AdaptiveRateLimiter::new(config.clone());
        limiter.penalize(None);
        {
            let state = limiter.state.lock().unwrap();
            assert_eq!(state.current_interval, Duration::from_millis(1000));
        }

        limiter.relax();
        {
            let state = limiter.state.lock().unwrap();
            assert_eq!(state.current_interval, Duration::from_millis(900));
        }
    }

    #[test]
    fn test_is_available() {
        let mut config = RateLimitConfig::default();
        config.requests_per_day = 1;
        let limiter = AdaptiveRateLimiter::new(config);
        assert!(limiter.is_available());
        limiter.wait();
        assert!(!limiter.is_available());
    }
}
