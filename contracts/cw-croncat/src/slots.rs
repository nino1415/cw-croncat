use crate::traits::IntervalExt;
use crate::CwCroncat;
use cosmwasm_std::{BlockInfo, Env, Order, StdResult, Storage};
use cron_schedule::Schedule;
pub use cw_croncat_core::types::Interval;
use cw_croncat_core::types::{Boundary, BoundarySpec, SlotType};
use std::str::FromStr;

fn get_next_block_limited(env: Env, boundary: Boundary) -> (u64, SlotType) {
    let current_block_height = env.block.height;

    let next_block_height = if boundary.start.is_some() {
        match boundary.start.unwrap() {
            // Note: Not bothering with time, as that should get handled with the cron situations,
            // and probably throw an error when mixing blocks and cron
            BoundarySpec::Height(id) => {
                if current_block_height < id {
                    // shorthand - remove 1 since it adds 1 later
                    id - 1
                } else {
                    current_block_height
                }
            }
            _ => current_block_height,
        }
    } else {
        current_block_height
    };

    if boundary.end.is_some() {
        match boundary.end.unwrap() {
            BoundarySpec::Height(id) => {
                // stop if passed end height
                if current_block_height > id {
                    return (0, SlotType::Block);
                }
                // we ONLY want to catch if we're passed the end block height
                if next_block_height > id {
                    return (id, SlotType::Block);
                }
            }
            _ => unreachable!(),
        }
    }

    // immediate needs to return this block + 1
    (next_block_height + 1, SlotType::Block)
}

// So either:
// - Boundary specifies a start/end that block offsets can be computed from
// - Block offset will truncate to specific modulo offsets
fn get_next_block_by_offset(env: Env, boundary: Boundary, block: u64) -> (u64, SlotType) {
    let current_block_height = env.block.height;
    let modulo_block = current_block_height.saturating_sub(current_block_height % block) + block;

    let next_block_height = if boundary.start.is_some() {
        match boundary.start.unwrap() {
            // Note: Not bothering with time, as that should get handled with the cron situations,
            // and probably throw an error when mixing blocks and cron
            BoundarySpec::Height(id) => {
                if current_block_height < id {
                    let rem = id % block;
                    if rem > 0 {
                        id.saturating_sub(rem) + block
                    } else {
                        id
                    }
                } else {
                    modulo_block
                }
            }
            _ => modulo_block,
        }
    } else {
        modulo_block
    };

    if boundary.end.is_some() {
        match boundary.end.unwrap() {
            BoundarySpec::Height(id) => {
                let rem = id % block;
                let end_height = if rem > 0 { id.saturating_sub(rem) } else { id };

                // stop if passed end height
                if current_block_height > id {
                    return (0, SlotType::Block);
                }

                // we ONLY want to catch if we're passed the end block height
                if next_block_height > end_height {
                    return (end_height, SlotType::Block);
                }
            }
            _ => unreachable!(),
        }
    }

    (next_block_height, SlotType::Block)
}

impl IntervalExt for Interval {
    fn next(&self, env: Env, boundary: Boundary) -> (u64, SlotType) {
        match self {
            // return the first block within a specific range that can be triggered 1 time.
            Interval::Once => get_next_block_limited(env, boundary),
            // return the first block within a specific range that can be triggered immediately, potentially multiple times.
            Interval::Immediate => get_next_block_limited(env, boundary),
            // return the first block within a specific range that can be triggered 1 or more times based on timestamps.
            // Uses crontab spec
            Interval::Cron(crontab) => {
                let current_block_ts: u64 = env.block.time.nanos();
                // TODO: get current timestamp within boundary
                let current_ts: u64 = if boundary.start.is_some() {
                    let start = boundary.start.unwrap();

                    match start {
                        // Note: Not bothering with height, as that should get handled with the block situations,
                        // and probably throw an error when mixing blocks and cron
                        BoundarySpec::Time(ts) => {
                            if current_block_ts < ts.nanos() {
                                ts.nanos()
                            } else {
                                current_block_ts
                            }
                        }
                        _ => current_block_ts,
                    }
                } else {
                    current_block_ts
                };

                let schedule = Schedule::from_str(crontab.as_str()).unwrap();
                let next_ts = schedule.next_after(&current_ts).unwrap();
                (next_ts, SlotType::Cron)
            }
            // return the block within a specific range that can be triggered 1 or more times based on block heights.
            // Uses block offset (Example: Block(100) will trigger every 100 blocks)
            // So either:
            // - Boundary specifies a start/end that block offsets can be computed from
            // - Block offset will truncate to specific modulo offsets
            Interval::Block(block) => get_next_block_by_offset(env, boundary, *block),
        }
    }
    fn is_valid(&self) -> bool {
        match self {
            Interval::Once => true,
            Interval::Immediate => true,
            Interval::Block(_) => true,
            Interval::Cron(crontab) => {
                let s = Schedule::from_str(crontab);
                s.is_ok()
            }
        }
    }
}

impl<'a> CwCroncat<'a> {
    /// Get the slot with lowest height/timestamp
    /// Returns (block slot, time slot)
    /// NOTE: This prioritizes blocks over timestamps.
    pub(crate) fn get_current_slot_items(
        &self,
        block: &BlockInfo,
        storage: &dyn Storage,
    ) -> Option<(u64, SlotType)> {
        let block_height = block.height;
        let block_slot: StdResult<Vec<u64>> = self
            .block_slots
            .keys(storage, None, None, Order::Ascending)
            .take(1)
            .collect();

        if let Ok(Some(block_id)) = block_slot.map(|v| v.first().copied()) {
            if block_height >= block_id {
                return Some((block_id, SlotType::Block));
            }
        }

        let timestamp: u64 = block.time.nanos();
        let time_slot: StdResult<Vec<u64>> = self
            .time_slots
            .keys(storage, None, None, Order::Ascending)
            .take(1)
            .collect();

        if let Ok(Some(time_id)) = time_slot.map(|v| v.first().copied()) {
            if timestamp >= time_id {
                return Some((time_id, SlotType::Cron));
            }
        }

        None
    }

    /// Gets 1 slot hash item, and removes the hash from storage
    /// Cleans up a slot if empty
    pub(crate) fn pop_slot_item(
        &mut self,
        storage: &mut dyn Storage,
        slot: &u64,
        kind: &SlotType,
    ) -> Option<Vec<u8>> {
        let store = match kind {
            SlotType::Block => self.block_slots.clone(),
            SlotType::Cron => self.time_slots.clone(),
        };
        println!("slot in pop_slot_item: {:?}", slot);
        let mut slot_data = store.may_load(storage, *slot).unwrap()?;

        // Get a single task hash, then retrieve task details
        let hash = slot_data.pop();

        // Need to remove this slot if no hash's left
        if slot_data.is_empty() {
            println!("clean_slot IS CALLED");
            self.clean_slot(storage, slot, kind);
        } else {
            store
                .update(storage, *slot, |_d| -> StdResult<Vec<_>> { Ok(slot_data) })
                .ok();
            println!("clean_slot ISN'T CALLED");
        }
        println!("12346 in pop_slot_item {:?}", store.load(storage, 12346));
        if hash.is_some() {
            return hash;
        }
        None
    }

    // TODO: TestCov
    /// Used in cases where there are empty slots or failed txns
    fn clean_slot(&mut self, storage: &mut dyn Storage, slot: &u64, kind: &SlotType) {
        let store = match kind {
            SlotType::Block => self.block_slots.clone(),
            SlotType::Cron => self.time_slots.clone(),
        };

        println!("12346 before {:?}", store.load(storage, 12346));
        println!("12347 before {:?}", store.load(storage, 12347));
        store.remove(storage, *slot);
        println!("12346 after {:?}", store.load(storage, 12346));
        println!("12347 after {:?}", store.load(storage, 12347));
    }
}

#[rustfmt::skip]
#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::{testing::{mock_env, mock_dependencies_with_balance}, coins};

    #[test]
    fn interval_get_next_block_limited() {
        // (input, input, outcome, outcome)
        let cases: Vec<(Interval, Boundary, u64, SlotType)> = vec![
            // Once cases
            (Interval::Once, Boundary { start: None, end: None }, 12346, SlotType::Block),
            (Interval::Once, Boundary { start: Some(BoundarySpec::Height(12348)), end: None }, 12348, SlotType::Block),
            (Interval::Once, Boundary { start: None, end: Some(BoundarySpec::Height(12346)) }, 12346, SlotType::Block),
            (Interval::Once, Boundary { start: None, end: Some(BoundarySpec::Height(12340)) }, 0, SlotType::Block),
            // Immediate cases
            (Interval::Immediate, Boundary { start: None, end: None }, 12346, SlotType::Block),
            (Interval::Immediate, Boundary { start: Some(BoundarySpec::Height(12348)), end: None }, 12348, SlotType::Block),
            (Interval::Immediate, Boundary { start: None, end: Some(BoundarySpec::Height(12346)) }, 12346, SlotType::Block),
            (Interval::Immediate, Boundary { start: None, end: Some(BoundarySpec::Height(12340)) }, 0, SlotType::Block),
        ];
        for (interval, boundary, outcome_block, outcome_slot_kind) in cases.iter() {
            let env = mock_env();
            // CHECK IT!
            let (next_id, slot_kind) = interval.next(env, boundary.clone());
            println!("next_id {:?}, slot_kind {:?}", next_id, slot_kind);
            assert_eq!(outcome_block, &next_id);
            assert_eq!(outcome_slot_kind, &slot_kind);
        }
    }

    #[test]
    fn interval_get_next_block_by_offset() {
        // (input, input, outcome, outcome)
        let cases: Vec<(Interval, Boundary, u64, SlotType)> = vec![
            // strictly modulo cases
            (Interval::Block(1), Boundary { start: None, end: None }, 12346, SlotType::Block),
            (Interval::Block(10), Boundary { start: None, end: None }, 12350, SlotType::Block),
            (Interval::Block(100), Boundary { start: None, end: None }, 12400, SlotType::Block),
            (Interval::Block(1000), Boundary { start: None, end: None }, 13000, SlotType::Block),
            (Interval::Block(10000), Boundary { start: None, end: None }, 20000, SlotType::Block),
            (Interval::Block(100000), Boundary { start: None, end: None }, 100000, SlotType::Block),
            // modulo + boundary start
            (Interval::Block(1), Boundary { start: Some(BoundarySpec::Height(12348)), end: None }, 12348, SlotType::Block),
            (Interval::Block(10), Boundary { start: Some(BoundarySpec::Height(12360)), end: None }, 12360, SlotType::Block),
            (Interval::Block(10), Boundary { start: Some(BoundarySpec::Height(12364)), end: None }, 12370, SlotType::Block),
            (Interval::Block(100), Boundary { start: Some(BoundarySpec::Height(12364)), end: None }, 12400, SlotType::Block),
            // modulo + boundary end
            (Interval::Block(1), Boundary { start: None, end: Some(BoundarySpec::Height(12345)) }, 12345, SlotType::Block),
            (Interval::Block(10), Boundary { start: None, end: Some(BoundarySpec::Height(12355)) }, 12350, SlotType::Block),
            (Interval::Block(100), Boundary { start: None, end: Some(BoundarySpec::Height(12355)) }, 12300, SlotType::Block),
            (Interval::Block(100), Boundary { start: None, end: Some(BoundarySpec::Height(12300)) }, 0, SlotType::Block),
        ];
        for (interval, boundary, outcome_block, outcome_slot_kind) in cases.iter() {
            let env = mock_env();
            // CHECK IT!
            let (next_id, slot_kind) = interval.next(env, boundary.clone());
            assert_eq!(outcome_block, &next_id);
            assert_eq!(outcome_slot_kind, &slot_kind);
        }
    }

    #[test]
    fn slot_items_get_current() {
        let mut deps = mock_dependencies_with_balance(&coins(200, ""));
        let store = CwCroncat::default();
        let mock_env = mock_env();
        let current_block = mock_env.block.height;
        let current_time = mock_env.block.time.nanos();
        let task_hash = "ad15b0f15010d57a51ff889d3400fe8d083a0dab2acfc752c5eb55e9e6281705"
            .as_bytes()
            .to_vec();

        // Check for empty store
        assert_eq!(None, store.get_current_slot_items(&mock_env.block, &deps.storage));

        // Setup of block and cron slots
        store.time_slots.save(&mut deps.storage, current_time + 1, &vec![task_hash.clone()]).unwrap();
        store.block_slots.save(&mut deps.storage, current_block + 1, &vec![task_hash.clone()]).unwrap();

        // Empty if not time/block yet
        assert_eq!(None, store.get_current_slot_items(&mock_env.block, &deps.storage));

        // And returns task when it's time
        let mut mock_env = mock_env;
        mock_env.block.time = mock_env.block.time.plus_nanos(1);
        assert_eq!(Some((current_time + 1, SlotType::Cron)),store.get_current_slot_items(&mock_env.block, &deps.storage));

        // Or later
        mock_env.block.time = mock_env.block.time.plus_nanos(1);
        assert_eq!(Some((current_time + 1, SlotType::Cron)),store.get_current_slot_items(&mock_env.block, &deps.storage));

        // Check, that Block is preferred over cron and block height reached 
        mock_env.block.height += 1;
        assert_eq!(Some((current_block + 1, SlotType::Block)),store.get_current_slot_items(&mock_env.block, &deps.storage));

        // Or block(s) ahead
        mock_env.block.height += 1;
        assert_eq!(Some((current_block + 1, SlotType::Block)),store.get_current_slot_items(&mock_env.block, &deps.storage));
    }

    #[test]
    fn slot_items_pop() {
        let mut deps = mock_dependencies_with_balance(&coins(200, ""));
        let mut store = CwCroncat::default();

        // Empty slots
        store.time_slots.save(&mut deps.storage, 0, &vec![]).unwrap();
        store.block_slots.save(&mut deps.storage, 0, &vec![]).unwrap();
        assert_eq!(None, store.pop_slot_item(&mut deps.storage, &0, &SlotType::Cron));
        assert_eq!(None, store.pop_slot_item(&mut deps.storage, &0, &SlotType::Block));

        // Just checking mutiple tasks
        let multiple_tasks = vec![
            "task_1".as_bytes().to_vec(),
            "task_2".as_bytes().to_vec(),
            "task_3".as_bytes().to_vec()
        ];
        store.time_slots.save(&mut deps.storage, 1, &multiple_tasks).unwrap();
        store.block_slots.save(&mut deps.storage, 1, &multiple_tasks).unwrap();
        for task in multiple_tasks.iter().rev() {
            assert_eq!(*task, store.pop_slot_item(&mut deps.storage, &1, &SlotType::Cron).unwrap());
            assert_eq!(*task, store.pop_slot_item(&mut deps.storage, &1, &SlotType::Block).unwrap());
        }

        // Slot removed if no hash left
        assert_eq!(None, store.pop_slot_item(&mut deps.storage, &1, &SlotType::Cron));
        assert_eq!(None, store.pop_slot_item(&mut deps.storage, &1, &SlotType::Block));
    }
}
