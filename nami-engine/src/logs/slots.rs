//! Per-user counting slots: how many of one kind of request each user has
//! running (entry reads, see [`super::search::MAX_ENTRY_READS_PER_USER`];
//! exports, see [`super::export::MAX_EXPORTS_PER_USER`]), and optionally how
//! many run in all.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use uuid::Uuid;

use crate::error::AppError;

/// How many requests each user has running, and how many in all.
#[derive(Debug, Default)]
pub struct UserSlots {
    counts: Mutex<Counts>,
}

#[derive(Debug, Default)]
struct Counts {
    per_user: HashMap<Uuid, usize>,
    /// Every permit out, all users together.
    total: usize,
}

/// One running request; dropping it gives the slot back.
#[derive(Debug)]
pub struct UserPermit {
    slots: Arc<UserSlots>,
    user_id: Uuid,
}

impl UserSlots {
    /// A slot for `user_id`, or `RateLimited` (429) when the user already has
    /// `max` running.
    pub fn acquire(self: &Arc<Self>, user_id: Uuid, max: usize) -> Result<UserPermit, AppError> {
        self.acquire_capped(user_id, max, usize::MAX)
    }

    /// As [`Self::acquire`], and also refused (429) once `total` permits of
    /// every user together are out (exports, see
    /// [`super::export::MAX_EXPORTS_PER_INSTANCE`]).
    pub fn acquire_capped(
        self: &Arc<Self>,
        user_id: Uuid,
        per_user: usize,
        total: usize,
    ) -> Result<UserPermit, AppError> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if counts.total >= total {
            return Err(AppError::RateLimited);
        }
        let count = counts.per_user.entry(user_id).or_insert(0);
        if *count >= per_user {
            return Err(AppError::RateLimited);
        }
        *count += 1;
        counts.total += 1;
        Ok(UserPermit {
            slots: self.clone(),
            user_id,
        })
    }

    /// Requests `user_id` has running.
    pub fn open(&self, user_id: Uuid) -> usize {
        let counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.per_user.get(&user_id).copied().unwrap_or(0)
    }

    /// Requests running, all users together.
    pub fn total(&self) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .total
    }
}

impl Drop for UserPermit {
    fn drop(&mut self) {
        let mut counts = self
            .slots
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        counts.total = counts.total.saturating_sub(1);
        if let Some(count) = counts.per_user.get_mut(&self.user_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.per_user.remove(&self.user_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_cap_each_user_and_come_back_on_drop() {
        let slots = Arc::new(UserSlots::default());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = slots.acquire(a, 2).unwrap();
        let _second = slots.acquire(a, 2).unwrap();
        assert!(matches!(slots.acquire(a, 2), Err(AppError::RateLimited)));
        let _other = slots.acquire(b, 2).unwrap();
        drop(first);
        assert_eq!(slots.open(a), 1);
        assert!(slots.acquire(a, 2).is_ok());
    }

    #[test]
    fn a_total_caps_every_user_together() {
        let slots = Arc::new(UserSlots::default());
        let held: Vec<_> = (0..3)
            .map(|_| slots.acquire_capped(Uuid::new_v4(), 2, 3).unwrap())
            .collect();
        assert!(matches!(
            slots.acquire_capped(Uuid::new_v4(), 2, 3),
            Err(AppError::RateLimited)
        ));
        drop(held);
        assert!(slots.acquire_capped(Uuid::new_v4(), 2, 3).is_ok());
    }

    #[test]
    fn a_capped_permit_still_counts_against_its_user() {
        let slots = Arc::new(UserSlots::default());
        let user = Uuid::new_v4();
        let _a = slots.acquire_capped(user, 2, 8).unwrap();
        let _b = slots.acquire_capped(user, 2, 8).unwrap();
        assert!(matches!(
            slots.acquire_capped(user, 2, 8),
            Err(AppError::RateLimited)
        ));
        assert_eq!(slots.open(user), 2);
        assert_eq!(slots.total(), 2);
    }
}
