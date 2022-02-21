use super::list::{IterRef, Skiplist};
use crate::common::format::{pack_sequence_and_type, ValueType};
use crate::common::InternalKeyComparator;
use crate::table::InternalIterator;
use crate::util::extract_user_key;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub struct Memtable {
    list: Skiplist,
    mem_next_logfile_number: AtomicU64,
    id: u64,
    comparator: InternalKeyComparator,
    pending_writes: AtomicU64,
    pending_schedule: AtomicBool,
    max_write_buffer_size: usize,
}

pub struct MemIterator {
    inner: IterRef<Skiplist>,
}

impl Memtable {
    pub fn new(id: u64, max_write_buffer_size: usize, comparator: InternalKeyComparator) -> Self {
        Self {
            list: Skiplist::with_capacity(comparator.clone(), 4 * 1024 * 1024),
            comparator,
            pending_writes: AtomicU64::new(0),
            mem_next_logfile_number: AtomicU64::new(0),
            id,
            pending_schedule: AtomicBool::new(false),
            max_write_buffer_size,
        }
    }

    pub fn new_iterator(&self) -> Box<dyn InternalIterator> {
        let iter = self.list.iter();
        Box::new(MemIterator { inner: iter })
    }

    pub fn get_comparator(&self) -> InternalKeyComparator {
        self.comparator.clone()
    }

    pub fn add(&self, key: &[u8], value: &[u8], sequence: u64, tp: ValueType) {
        let mut ukey = Vec::with_capacity(key.len() + 8);
        ukey.extend_from_slice(key);
        ukey.extend_from_slice(&pack_sequence_and_type(sequence, tp as u8).to_le_bytes());
        self.insert_to(ukey.into(), value.into());
    }

    pub fn delete(&self, key: &[u8], sequence: u64) {
        let mut ukey = Vec::with_capacity(key.len() + 8);
        ukey.extend_from_slice(key);
        ukey.extend_from_slice(
            &pack_sequence_and_type(sequence, ValueType::TypeDeletion as u8).to_le_bytes(),
        );
        self.insert_to(ukey.into(), vec![]);
    }

    pub fn insert(&self, key: &[u8], value: &[u8]) {
        self.insert_to(key.into(), value.into());
    }

    pub fn insert_to(&self, key: Vec<u8>, value: Vec<u8>) {
        self.list.put(key, value);
    }

    pub fn get_id(&self) -> u64 {
        self.id
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut iter = self.list.iter();
        iter.seek(key);
        if iter.valid()
            && self
                .comparator
                .get_user_comparator()
                .same_key(extract_user_key(key), extract_user_key(iter.key()))
        {
            return Some(iter.value().to_vec());
        }
        None
    }

    pub fn set_next_log_number(&self, num: u64) {
        self.mem_next_logfile_number.store(num, Ordering::Release);
    }

    pub fn get_next_log_number(&self) -> u64 {
        self.mem_next_logfile_number.load(Ordering::Acquire)
    }

    // TODO: support write buffer manager
    pub fn should_flush(&self) -> bool {
        self.list.mem_size() as usize > self.max_write_buffer_size
    }

    // return true if this memtable can be scheduled flush at once
    pub fn mark_schedule_flush(&self) -> bool {
        self.pending_schedule.store(true, Ordering::Release);
        self.pending_writes.load(Ordering::Acquire) == 0
    }

    pub fn mark_write_begin(&self, count: u64) {
        self.pending_writes.fetch_add(count, Ordering::SeqCst);
    }

    // return true if this memtable must be scheduled flush at once
    pub fn mark_write_done(&self) -> bool {
        if self.pending_writes.fetch_sub(1, Ordering::SeqCst) - 1 == 0 {
            self.pending_schedule.load(Ordering::Acquire)
        } else {
            false
        }
    }
}

impl InternalIterator for MemIterator {
    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn seek(&mut self, key: &[u8]) {
        self.inner.seek(key)
    }

    fn seek_to_first(&mut self) {
        self.inner.seek_to_first();
    }

    fn seek_to_last(&mut self) {
        self.inner.seek_to_last()
    }

    fn seek_for_prev(&mut self, key: &[u8]) {
        self.inner.seek_for_prev(key)
    }

    fn next(&mut self) {
        self.inner.next()
    }

    fn prev(&mut self) {
        self.inner.prev()
    }

    fn key(&self) -> &[u8] {
        self.inner.key().as_ref()
    }

    fn value(&self) -> &[u8] {
        self.inner.value().as_ref()
    }
}
