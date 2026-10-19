//! Per-user counting slots: how many of one kind of request each user has
//! running (entry reads, see [`super::search::MAX_ENTRY_READS_PER_USER`]).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use uuid::Uuid;

use crate::error::AppError;

/// How many requests each user has running.
#[derive(Debug, Default)]
pub struct UserSlots {
    per_user: Mutex<HashMap<Uuid, usize>>,
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
        let mut counts = self.per_user.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(user_id).or_insert(0);
        if *count >= max {
            return Err(AppError::RateLimited);
        }
        *count += 1;
        Ok(UserPermit {
            slots: self.clone(),
            user_id,
        })
    }

    /// Requests `user_id` has running.
    pub fn open(&self, user_id: Uuid) -> usize {
        let counts = self.per_user.lock().unwrap_or_else(PoisonError::into_inner);
        counts.get(&user_id).copied().unwrap_or(0)
    }
}

impl Drop for UserPermit {
    fn drop(&mut self) {
        let mut counts = self
            .slots
            .per_user
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.user_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.user_id);
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
}
