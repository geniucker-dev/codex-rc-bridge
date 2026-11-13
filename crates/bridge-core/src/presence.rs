use tokio::time::{Duration, Instant};
#[derive(Clone, Default)]
pub struct Presence {
    online: bool,
    reported_offline: bool,
    checked: Option<Instant>,
    activity: Option<Instant>,
}

impl Presence {
    pub fn online(&self) -> bool {
        self.online
    }
    pub fn confirmed_offline(&self) -> bool {
        self.reported_offline
    }
    pub fn report(&mut self, online: bool, now: Instant) {
        self.reported_offline = !online;
        self.checked = Some(now);
        self.online = online;
        if !online {
            self.activity = None;
        }
    }
    /// Only Delivery's newly forwarded complete data/active pong can reach this method.
    pub fn activity(&mut self, now: Instant) {
        if self.online && self.checked.is_some() {
            self.activity = Some(now);
        }
    }
    pub fn expire(&mut self, now: Instant) {
        if self.online
            && self
                .checked
                .is_some_and(|at| now.duration_since(at) >= Duration::from_secs(30))
            && self
                .activity
                .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(90))
        {
            self.online = false;
        }
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    #[test]
    fn missing_or_expired_evidence_is_not_an_explicit_offline_report() {
        let mut presence = Presence::default();
        let now = Instant::now();
        assert!(!presence.online());
        assert!(!presence.confirmed_offline());
        presence.report(false, now);
        assert!(presence.confirmed_offline());
        presence.activity(now);
        assert!(!presence.online());
        presence.report(true, now);
        presence.expire(now + Duration::from_secs(31));
        assert!(!presence.online());
        assert!(!presence.confirmed_offline());
    }
}
