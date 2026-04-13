use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Original Store (preserved)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct MemEntry {
    pub key: String,
    pub value: String,
    pub version: u32,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
struct InternalEntry {
    entry: MemEntry,
    created_at: u64,
    ttl_secs: u64,
}

impl InternalEntry {
    fn is_expired(&self) -> bool {
        if self.ttl_secs == 0 {
            return false;
        }
        now_epoch() > self.created_at + self.ttl_secs
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub entries: Vec<(String, MemEntry)>,
    pub label: String,
}

pub struct Store {
    entries: HashMap<String, InternalEntry>,
}

impl Store {
    pub fn new() -> Self {
        Store {
            entries: HashMap::new(),
        }
    }

    pub fn put(&mut self, key: &str, value: &str, ttl_secs: u64, read_only: bool) {
        let existing = self
            .entries
            .get(key)
            .map(|ie| ie.entry.version)
            .unwrap_or(0);
        let entry = MemEntry {
            key: key.to_string(),
            value: value.to_string(),
            version: existing + 1,
            read_only,
        };
        self.entries.insert(
            key.to_string(),
            InternalEntry {
                entry,
                created_at: now_epoch(),
                ttl_secs,
            },
        );
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.entries.get(key).and_then(|ie| {
            if ie.is_expired() {
                None
            } else {
                Some(ie.entry.value.clone())
            }
        })
    }

    pub fn delete(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    pub fn exists(&self, key: &str) -> bool {
        self.entries
            .get(key)
            .map_or(false, |ie| !ie.is_expired())
    }

    pub fn update(&mut self, key: &str, value: &str) -> bool {
        let ie = match self.entries.get_mut(key) {
            Some(ie) if !ie.is_expired() => ie,
            _ => return false,
        };
        if ie.entry.read_only {
            return false;
        }
        ie.entry.value = value.to_string();
        ie.entry.version += 1;
        true
    }

    pub fn search(&self, prefix: &str) -> Vec<&MemEntry> {
        self.entries
            .iter()
            .filter(|(k, ie)| k.starts_with(prefix) && !ie.is_expired())
            .map(|(_, ie)| &ie.entry)
            .collect()
    }

    pub fn count(&self) -> usize {
        self.entries.values().filter(|ie| !ie.is_expired()).count()
    }

    pub fn gc(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, ie| !ie.is_expired());
        before - self.entries.len()
    }

    pub fn snapshot(&self, label: &str) -> Snapshot {
        let entries: Vec<(String, MemEntry)> = self
            .entries
            .iter()
            .filter(|(_, ie)| !ie.is_expired())
            .map(|(k, ie)| (k.clone(), ie.entry.clone()))
            .collect();
        Snapshot {
            entries,
            label: label.to_string(),
        }
    }

    pub fn restore(&mut self, snap: &Snapshot) {
        self.entries.clear();
        for (k, entry) in &snap.entries {
            self.entries.insert(
                k.clone(),
                InternalEntry {
                    entry: entry.clone(),
                    created_at: now_epoch(),
                    ttl_secs: 0,
                },
            );
        }
    }

    pub fn diff(&self, snap: &Snapshot) -> (Vec<String>, Vec<String>) {
        let snap_keys: std::collections::HashSet<&str> =
            snap.entries.iter().map(|(k, _)| k.as_str()).collect();
        let cur_keys: std::collections::HashSet<&str> = self
            .entries
            .iter()
            .filter(|(_, ie)| !ie.is_expired())
            .map(|(k, _)| k.as_str())
            .collect();

        let added: Vec<String> = cur_keys
            .difference(&snap_keys)
            .map(|s| s.to_string())
            .collect();
        let removed: Vec<String> = snap_keys
            .difference(&cur_keys)
            .map(|s| s.to_string())
            .collect();
        (added, removed)
    }
}

// ---------------------------------------------------------------------------
// Memory Pool Allocator (fixed-size block pools)
// ---------------------------------------------------------------------------

/// A fixed-size block memory pool. Allocates blocks of uniform `block_size`.
/// Returns block indices on allocation. O(1) alloc and free via free-list.
pub struct PoolAllocator {
    buffer: Vec<u8>,
    block_size: usize,
    capacity: usize,
    free_list: Vec<usize>,
    allocated: std::collections::HashSet<usize>,
    total_allocations: u64,
    total_deallocations: u64,
}

impl PoolAllocator {
    /// Create a new pool with the given block size and number of blocks.
    pub fn new(block_size: usize, num_blocks: usize) -> Self {
        let capacity = block_size * num_blocks;
        let mut free_list = Vec::with_capacity(num_blocks);
        for i in 0..num_blocks {
            free_list.push(i);
        }
        free_list.reverse(); // so pop() returns lowest index first
        PoolAllocator {
            buffer: vec![0u8; capacity],
            block_size,
            capacity: num_blocks,
            free_list,
            allocated: std::collections::HashSet::new(),
            total_allocations: 0,
            total_deallocations: 0,
        }
    }

    /// Allocate a block, returning its index or None if pool is exhausted.
    pub fn allocate(&mut self) -> Option<usize> {
        let idx = self.free_list.pop()?;
        self.allocated.insert(idx);
        self.total_allocations += 1;
        Some(idx)
    }

    /// Free a previously allocated block by index.
    pub fn deallocate(&mut self, idx: usize) -> bool {
        if self.allocated.remove(&idx) {
            self.free_list.push(idx);
            // Zero out the block for safety
            let start = idx * self.block_size;
            let end = start + self.block_size;
            if end <= self.buffer.len() {
                for b in &mut self.buffer[start..end] {
                    *b = 0;
                }
            }
            self.total_deallocations += 1;
            true
        } else {
            false
        }
    }

    /// Write data into an allocated block. Returns false if block is not allocated or data doesn't fit.
    pub fn write(&mut self, idx: usize, data: &[u8]) -> bool {
        if !self.allocated.contains(&idx) || data.len() > self.block_size {
            return false;
        }
        let start = idx * self.block_size;
        self.buffer[start..start + data.len()].copy_from_slice(data);
        true
    }

    /// Read data from an allocated block into a destination slice.
    pub fn read(&self, idx: usize, buf: &mut [u8]) -> bool {
        if !self.allocated.contains(&idx) || buf.len() > self.block_size {
            return false;
        }
        let start = idx * self.block_size;
        buf.copy_from_slice(&self.buffer[start..start + buf.len()]);
        true
    }

    /// Number of free blocks remaining.
    pub fn available(&self) -> usize {
        self.free_list.len()
    }

    /// Number of currently allocated blocks.
    pub fn in_use(&self) -> usize {
        self.allocated.len()
    }

    /// Total capacity in blocks.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Total allocations performed over lifetime.
    pub fn total_allocations(&self) -> u64 {
        self.total_allocations
    }

    /// Total deallocations performed over lifetime.
    pub fn total_deallocations(&self) -> u64 {
        self.total_deallocations
    }
}

// ---------------------------------------------------------------------------
// Stack Allocator (LIFO allocation pattern)
// ---------------------------------------------------------------------------

/// A stack allocator that hands out memory regions from the top of a buffer.
/// Deallocation is LIFO only: you must free in reverse order of allocation.
pub struct StackAllocator {
    buffer: Vec<u8>,
    offset: usize,
    // Track allocation offsets for validation
    alloc_stack: Vec<(usize, usize)>, // (offset, size)
    total_allocations: u64,
    total_deallocations: u64,
}

impl StackAllocator {
    pub fn new(size: usize) -> Self {
        StackAllocator {
            buffer: vec![0u8; size],
            offset: 0,
            alloc_stack: Vec::new(),
            total_allocations: 0,
            total_deallocations: 0,
        }
    }

    /// Allocate `size` bytes, returning the offset into the buffer, or None if full.
    pub fn alloc(&mut self, size: usize) -> Option<usize> {
        let aligned_size = (size + 7) & !7; // 8-byte alignment
        if self.offset + aligned_size > self.buffer.len() {
            return None;
        }
        let start = self.offset;
        self.offset += aligned_size;
        self.alloc_stack.push((start, aligned_size));
        self.total_allocations += 1;
        Some(start)
    }

    /// Allocate and get a mutable slice. Convenience wrapper.
    pub fn alloc_slice(&mut self, size: usize) -> Option<&mut [u8]> {
        let offset = self.alloc(size)?;
        Some(&mut self.buffer[offset..offset + size])
    }

    /// Deallocate the most recent allocation (LIFO). Returns false if stack is empty.
    pub fn free(&mut self) -> bool {
        match self.alloc_stack.pop() {
            Some((start, size)) => {
                // Zero out for safety
                for b in &mut self.buffer[start..start + size] {
                    *b = 0;
                }
                self.offset = start;
                self.total_deallocations += 1;
                true
            }
            None => false,
        }
    }

    /// Get a reference to a previously allocated region.
    pub fn get_slice(&self, offset: usize, size: usize) -> Option<&[u8]> {
        if offset + size <= self.buffer.len() {
            Some(&self.buffer[offset..offset + size])
        } else {
            None
        }
    }

    /// Get a mutable reference to a previously allocated region.
    pub fn get_slice_mut(&mut self, offset: usize, size: usize) -> Option<&mut [u8]> {
        if offset + size <= self.buffer.len() {
            Some(&mut self.buffer[offset..offset + size])
        } else {
            None
        }
    }

    /// Current used bytes.
    pub fn used(&self) -> usize {
        self.offset
    }

    /// Total buffer size.
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Remaining free bytes.
    pub fn free_space(&self) -> usize {
        self.buffer.len() - self.offset
    }

    /// Depth of the allocation stack.
    pub fn depth(&self) -> usize {
        self.alloc_stack.len()
    }

    /// Total allocations over lifetime.
    pub fn total_allocations(&self) -> u64 {
        self.total_allocations
    }

    /// Total deallocations over lifetime.
    pub fn total_deallocations(&self) -> u64 {
        self.total_deallocations
    }

    /// Reset the allocator, clearing all allocations.
    pub fn reset(&mut self) {
        self.offset = 0;
        self.alloc_stack.clear();
        self.buffer.fill(0);
    }
}

// ---------------------------------------------------------------------------
// Arena Allocator (batch allocation, batch free)
// ---------------------------------------------------------------------------

/// An arena allocator that bumps a pointer forward. You can allocate many
/// objects, then free everything at once. Individual frees are not supported.
pub struct ArenaAllocator {
    chunks: Vec<Vec<u8>>,
    current_chunk: usize,
    current_offset: usize,
    chunk_size: usize,
    total_allocated_bytes: u64,
    allocation_count: u64,
}

impl ArenaAllocator {
    pub fn new(chunk_size: usize) -> Self {
        let first_chunk = vec![0u8; chunk_size];
        ArenaAllocator {
            chunks: vec![first_chunk],
            current_chunk: 0,
            current_offset: 0,
            chunk_size,
            total_allocated_bytes: 0,
            allocation_count: 0,
        }
    }

    /// Allocate `size` bytes from the arena. Returns an offset tuple (chunk_index, byte_offset).
    pub fn allocate(&mut self, size: usize) -> Option<(usize, usize)> {
        let aligned_size = (size + 7) & !7;
        if self.current_offset + aligned_size > self.chunk_size {
            // Need a new chunk
            self.chunks.push(vec![0u8; self.chunk_size]);
            self.current_chunk += 1;
            self.current_offset = 0;
        }
        let start = self.current_offset;
        self.current_offset += aligned_size;
        self.total_allocated_bytes += aligned_size as u64;
        self.allocation_count += 1;
        Some((self.current_chunk, start))
    }

    /// Allocate and return a mutable slice into the arena.
    pub fn allocate_slice(&mut self, size: usize) -> Option<&mut [u8]> {
        let (chunk_idx, offset) = self.allocate(size)?;
        Some(&mut self.chunks[chunk_idx][offset..offset + size])
    }

    /// Write data to an allocated region.
    pub fn write(&mut self, chunk_idx: usize, offset: usize, data: &[u8]) -> bool {
        if chunk_idx >= self.chunks.len() || offset + data.len() > self.chunk_size {
            return false;
        }
        self.chunks[chunk_idx][offset..offset + data.len()].copy_from_slice(data);
        true
    }

    /// Read data from an allocated region.
    pub fn read(&self, chunk_idx: usize, offset: usize, buf: &mut [u8]) -> bool {
        if chunk_idx >= self.chunks.len() || offset + buf.len() > self.chunk_size {
            return false;
        }
        buf.copy_from_slice(&self.chunks[chunk_idx][offset..offset + buf.len()]);
        true
    }

    /// Free everything at once. Clears all chunks.
    pub fn free_all(&mut self) {
        self.chunks.clear();
        self.chunks.push(vec![0u8; self.chunk_size]);
        self.current_chunk = 0;
        self.current_offset = 0;
    }

    /// Total bytes allocated over lifetime.
    pub fn total_allocated_bytes(&self) -> u64 {
        self.total_allocated_bytes
    }

    /// Number of individual allocations performed.
    pub fn allocation_count(&self) -> u64 {
        self.allocation_count
    }

    /// Number of chunks currently in use.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Current chunk size.
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }
}

// ---------------------------------------------------------------------------
// Memory Layout Planner
// ---------------------------------------------------------------------------

/// Describes a region to be placed in a memory layout.
#[derive(Clone, Debug)]
pub struct LayoutRegion {
    pub name: String,
    pub size: usize,
    pub alignment: usize,
    pub category: String,
}

/// A planned memory layout with calculated offsets.
#[derive(Clone, Debug)]
pub struct PlannedLayout {
    pub regions: Vec<PlannedRegion>,
    pub total_size: usize,
}

#[derive(Clone, Debug)]
pub struct PlannedRegion {
    pub name: String,
    pub size: usize,
    pub alignment: usize,
    pub offset: usize,
    pub padding_before: usize,
    pub category: String,
}

/// Planner that arranges data structures optimally, grouping by category
/// and applying alignment/padding rules.
pub struct MemoryLayoutPlanner;

impl MemoryLayoutPlanner {
    /// Plan a memory layout from a set of regions. Groups by category,
    /// sorts within each group by descending alignment for minimal padding.
    pub fn plan(regions: Vec<LayoutRegion>) -> PlannedLayout {
        let mut groups: Vec<Vec<LayoutRegion>> = Vec::new();
        let mut category_order: Vec<String> = Vec::new();

        for region in &regions {
            if let Some(pos) = category_order.iter().position(|c| c == &region.category) {
                groups[pos].push(region.clone());
            } else {
                category_order.push(region.category.clone());
                groups.push(vec![region.clone()]);
            }
        }

        // Sort within each group: larger alignment first
        for group in &mut groups {
            group.sort_by(|a, b| b.alignment.cmp(&a.alignment).then_with(|| b.size.cmp(&a.size)));
        }

        let mut planned = Vec::new();
        let mut offset: usize = 0;

        for group in groups {
            for region in group {
                let aligned_offset = (offset + region.alignment - 1) & !(region.alignment - 1);
                let padding = aligned_offset - offset;
                planned.push(PlannedRegion {
                    name: region.name,
                    size: region.size,
                    alignment: region.alignment,
                    offset: aligned_offset,
                    padding_before: padding,
                    category: region.category,
                });
                offset = aligned_offset + region.size;
            }
        }

        PlannedLayout {
            regions: planned,
            total_size: offset,
        }
    }

    /// Calculate the total wasted padding bytes in a layout.
    pub fn total_padding(layout: &PlannedLayout) -> usize {
        layout.regions.iter().map(|r| r.padding_before).sum()
    }

    /// Calculate layout efficiency as a percentage.
    pub fn efficiency(layout: &PlannedLayout) -> f64 {
        let used: usize = layout.regions.iter().map(|r| r.size).sum();
        if layout.total_size == 0 {
            return 100.0;
        }
        (used as f64 / layout.total_size as f64) * 100.0
    }
}

// ---------------------------------------------------------------------------
// Memory Safety Checker
// ---------------------------------------------------------------------------

/// Violation types detected by the safety checker.
#[derive(Clone, Debug, PartialEq)]
pub enum SafetyViolation {
    UseAfterFree {
        address: usize,
        freed_at_step: usize,
        accessed_at_step: usize,
    },
    BufferOverflow {
        address: usize,
        buffer_start: usize,
        buffer_size: usize,
        write_size: usize,
    },
    DoubleFree {
        address: usize,
        first_free_step: usize,
        second_free_step: usize,
    },
    Leak {
        address: usize,
        size: usize,
    },
}

/// A simulated memory region for tracking allocations.
#[derive(Clone, Debug)]
struct TrackedBlock {
    address: usize,
    size: usize,
    allocated: bool,
    allocated_at_step: usize,
    freed_at_step: Option<usize>,
}

/// Memory safety checker that simulates allocations and detects common
/// safety issues: use-after-free, buffer overflow, double free, and leaks.
pub struct MemorySafetyChecker {
    blocks: Vec<TrackedBlock>,
    next_address: usize,
    current_step: usize,
}

impl MemorySafetyChecker {
    pub fn new() -> Self {
        MemorySafetyChecker {
            blocks: Vec::new(),
            next_address: 1024, // Start at a non-zero address for realism
            current_step: 0,
        }
    }

    fn advance_step(&mut self) {
        self.current_step += 1;
    }

    /// Simulate an allocation. Returns the allocated address.
    pub fn alloc(&mut self, size: usize) -> usize {
        self.advance_step();
        let addr = self.next_address;
        self.next_address += size;
        self.blocks.push(TrackedBlock {
            address: addr,
            size,
            allocated: true,
            allocated_at_step: self.current_step,
            freed_at_step: None,
        });
        addr
    }

    /// Simulate freeing a block.
    pub fn free(&mut self, address: usize) -> Result<(), SafetyViolation> {
        self.advance_step();
        let block = self
            .blocks
            .iter_mut()
            .find(|b| b.address == address);

        match block {
            None => Ok(()), // Unknown address, no-op
            Some(b) => {
                if !b.allocated {
                    return Err(SafetyViolation::DoubleFree {
                        address,
                        first_free_step: b.freed_at_step.unwrap_or(0),
                        second_free_step: self.current_step,
                    });
                }
                b.allocated = false;
                b.freed_at_step = Some(self.current_step);
                Ok(())
            }
        }
    }

    /// Simulate a read access and check for use-after-free.
    pub fn read(&self, address: usize) -> Result<(), SafetyViolation> {
        let block = self.blocks.iter().find(|b| {
            address >= b.address && address < b.address + b.size
        });

        match block {
            None => Ok(()), // Outside any tracked block
            Some(b) => {
                if !b.allocated {
                    Err(SafetyViolation::UseAfterFree {
                        address,
                        freed_at_step: b.freed_at_step.unwrap_or(0),
                        accessed_at_step: self.current_step,
                    })
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Simulate a write and check for use-after-free and buffer overflow.
    pub fn write(&mut self, address: usize, write_size: usize) -> Result<(), SafetyViolation> {
        self.advance_step();
        let block = self.blocks.iter().find(|b| {
            address >= b.address && address < b.address + b.size
        });

        match block {
            None => {
                // Check if it overflows any allocated block
                for b in &self.blocks {
                    if b.allocated && address + write_size > b.address + b.size && address < b.address + b.size {
                        return Err(SafetyViolation::BufferOverflow {
                            address,
                            buffer_start: b.address,
                            buffer_size: b.size,
                            write_size,
                        });
                    }
                }
                Ok(())
            }
            Some(b) => {
                if !b.allocated {
                    return Err(SafetyViolation::UseAfterFree {
                        address,
                        freed_at_step: b.freed_at_step.unwrap_or(0),
                        accessed_at_step: self.current_step,
                    });
                }
                // Check overflow within the block
                let end_offset = (address - b.address) + write_size;
                if end_offset > b.size {
                    return Err(SafetyViolation::BufferOverflow {
                        address,
                        buffer_start: b.address,
                        buffer_size: b.size,
                        write_size,
                    });
                }
                Ok(())
            }
        }
    }

    /// Detect memory leaks: blocks that are still allocated at the end.
    pub fn detect_leaks(&self) -> Vec<SafetyViolation> {
        self.blocks
            .iter()
            .filter(|b| b.allocated)
            .map(|b| SafetyViolation::Leak {
                address: b.address,
                size: b.size,
            })
            .collect()
    }

    /// Run a full safety scan combining all checks.
    pub fn scan(&self) -> Vec<SafetyViolation> {
        self.detect_leaks()
    }

    /// Reset the checker.
    pub fn reset(&mut self) {
        self.blocks.clear();
        self.next_address = 1024;
        self.current_step = 0;
    }
}

// ---------------------------------------------------------------------------
// Memory Usage Tracker
// ---------------------------------------------------------------------------

/// Tracks memory usage across different categories and provides reporting.
#[derive(Clone, Debug)]
pub struct MemoryUsageRecord {
    pub category: String,
    pub allocated_bytes: u64,
    pub freed_bytes: u64,
    pub peak_bytes: u64,
    pub allocation_count: u64,
    pub deallocation_count: u64,
}

impl MemoryUsageRecord {
    pub fn current_usage(&self) -> u64 {
        self.allocated_bytes.saturating_sub(self.freed_bytes)
    }
}

/// A report of memory usage at a point in time.
#[derive(Clone, Debug)]
pub struct MemoryReport {
    pub records: Vec<MemoryUsageRecord>,
    pub total_current: u64,
    pub total_peak: u64,
    pub total_allocations: u64,
    pub timestamp: u64,
}

/// Memory usage tracker with per-category bookkeeping.
pub struct MemoryUsageTracker {
    records: HashMap<String, MemoryUsageRecord>,
    global_peak: u64,
    global_allocations: u64,
}

impl MemoryUsageTracker {
    pub fn new() -> Self {
        MemoryUsageTracker {
            records: HashMap::new(),
            global_peak: 0,
            global_allocations: 0,
        }
    }

    /// Record an allocation in a category.
    pub fn record_alloc(&mut self, category: &str, bytes: u64) {
        let record = self.records.entry(category.to_string()).or_insert_with(|| {
            MemoryUsageRecord {
                category: category.to_string(),
                allocated_bytes: 0,
                freed_bytes: 0,
                peak_bytes: 0,
                allocation_count: 0,
                deallocation_count: 0,
            }
        });
        record.allocated_bytes += bytes;
        record.allocation_count += 1;
        let current = record.current_usage();
        if current > record.peak_bytes {
            record.peak_bytes = current;
        }
        self.global_allocations += 1;

        // Update global peak
        let total: u64 = self.records.values().map(|r| r.current_usage()).sum();
        if total > self.global_peak {
            self.global_peak = total;
        }
    }

    /// Record a deallocation in a category.
    pub fn record_free(&mut self, category: &str, bytes: u64) {
        let record = self.records.entry(category.to_string()).or_insert_with(|| {
            MemoryUsageRecord {
                category: category.to_string(),
                allocated_bytes: 0,
                freed_bytes: 0,
                peak_bytes: 0,
                allocation_count: 0,
                deallocation_count: 0,
            }
        });
        record.freed_bytes += bytes;
        record.deallocation_count += 1;
    }

    /// Generate a memory usage report.
    pub fn report(&self) -> MemoryReport {
        let mut records: Vec<MemoryUsageRecord> = self.records.values().cloned().collect();
        records.sort_by(|a, b| b.current_usage().cmp(&a.current_usage()));
        let total_current: u64 = records.iter().map(|r| r.current_usage()).sum();
        let _total_peak: u64 = records.iter().map(|r| r.peak_bytes).max().unwrap_or(0);

        MemoryReport {
            records,
            total_current,
            total_peak: self.global_peak,
            total_allocations: self.global_allocations,
            timestamp: now_epoch(),
        }
    }

    /// Get current usage for a specific category.
    pub fn category_usage(&self, category: &str) -> u64 {
        self.records
            .get(category)
            .map(|r| r.current_usage())
            .unwrap_or(0)
    }

    /// Get peak usage for a specific category.
    pub fn category_peak(&self, category: &str) -> u64 {
        self.records
            .get(category)
            .map(|r| r.peak_bytes)
            .unwrap_or(0)
    }

    /// Reset all tracking.
    pub fn reset(&mut self) {
        self.records.clear();
        self.global_peak = 0;
        self.global_allocations = 0;
    }

    /// Number of tracked categories.
    pub fn category_count(&self) -> usize {
        self.records.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Original Store tests --

    #[test]
    fn test_put_and_get() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert_eq!(s.get("a"), Some("1".to_string()));
    }

    #[test]
    fn test_get_missing() {
        let s = Store::new();
        assert_eq!(s.get("nope"), None);
    }

    #[test]
    fn test_exists() {
        let mut s = Store::new();
        s.put("x", "v", 0, false);
        assert!(s.exists("x"));
        assert!(!s.exists("y"));
    }

    #[test]
    fn test_delete() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert!(s.delete("a"));
        assert!(!s.delete("a"));
        assert_eq!(s.get("a"), None);
    }

    #[test]
    fn test_update() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert!(s.update("a", "2"));
        assert_eq!(s.get("a"), Some("2".to_string()));
        assert_eq!(s.entries.get("a").unwrap().entry.version, 2);
    }

    #[test]
    fn test_update_readonly_fails() {
        let mut s = Store::new();
        s.put("a", "1", 0, true);
        assert!(!s.update("a", "2"));
        assert_eq!(s.get("a"), Some("1".to_string()));
    }

    #[test]
    fn test_snapshot_and_restore() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        let snap = s.snapshot("v1");
        s.put("c", "3", 0, false);
        s.delete("a");
        s.restore(&snap);
        assert_eq!(s.count(), 2);
        assert_eq!(s.get("a"), Some("1".to_string()));
    }

    // -- Pool Allocator tests --

    #[test]
    fn pool_allocates_and_frees() {
        let mut pool = PoolAllocator::new(64, 4);
        let idx = pool.allocate().expect("should allocate");
        assert!(pool.write(idx, b"hello world"));
        let mut buf = [0u8; 11];
        assert!(pool.read(idx, &mut buf));
        assert_eq!(&buf, b"hello world");
        assert!(pool.deallocate(idx));
        assert_eq!(pool.in_use(), 0);
    }

    #[test]
    fn pool_exhaustion() {
        let mut pool = PoolAllocator::new(32, 2);
        pool.allocate().unwrap();
        pool.allocate().unwrap();
        assert!(pool.allocate().is_none());
    }

    #[test]
    fn pool_double_deallocate_fails() {
        let mut pool = PoolAllocator::new(64, 2);
        let idx = pool.allocate().unwrap();
        assert!(pool.deallocate(idx));
        assert!(!pool.deallocate(idx));
    }

    #[test]
    fn pool_write_to_unallocated_fails() {
        let mut pool = PoolAllocator::new(64, 2);
        assert!(!pool.write(0, b"nope"));
    }

    #[test]
    fn pool_stats() {
        let mut pool = PoolAllocator::new(64, 4);
        assert_eq!(pool.available(), 4);
        pool.allocate().unwrap();
        pool.allocate().unwrap();
        assert_eq!(pool.available(), 2);
        assert_eq!(pool.in_use(), 2);
        assert_eq!(pool.total_allocations(), 2);
    }

    // -- Stack Allocator tests --

    #[test]
    fn stack_alloc_and_free() {
        let mut stack = StackAllocator::new(256);
        let off1 = stack.alloc(16).unwrap();
        let off2 = stack.alloc(32).unwrap();
        assert_ne!(off1, off2);
        assert!(stack.free()); // free off2
        assert!(stack.free()); // free off1
        assert_eq!(stack.used(), 0);
    }

    #[test]
    fn stack_lifo_enforced() {
        let mut stack = StackAllocator::new(256);
        stack.alloc(16).unwrap();
        stack.alloc(32).unwrap();
        assert!(stack.free());
        assert!(stack.free());
        assert!(!stack.free()); // stack empty
    }

    #[test]
    fn stack_exhaustion() {
        let mut stack = StackAllocator::new(32);
        stack.alloc(16).unwrap();
        assert!(stack.alloc(32).is_none()); // not enough room
    }

    #[test]
    fn stack_alignment() {
        let mut stack = StackAllocator::new(256);
        let off = stack.alloc(3).unwrap(); // 3 bytes, should be aligned to 8
        assert_eq!(off % 8, 0);
    }

    #[test]
    fn stack_reset() {
        let mut stack = StackAllocator::new(256);
        stack.alloc(64).unwrap();
        stack.alloc(64).unwrap();
        stack.reset();
        assert_eq!(stack.used(), 0);
        assert_eq!(stack.depth(), 0);
    }

    // -- Arena Allocator tests --

    #[test]
    fn arena_allocate_and_write() {
        let mut arena = ArenaAllocator::new(128);
        let (chunk, off) = arena.allocate(16).unwrap();
        assert!(arena.write(chunk, off, b"arena data"));
        let mut buf = [0u8; 10];
        assert!(arena.read(chunk, off, &mut buf));
        assert_eq!(&buf, b"arena data");
    }

    #[test]
    fn arena_multiple_allocations() {
        let mut arena = ArenaAllocator::new(64);
        arena.allocate(16).unwrap();
        arena.allocate(16).unwrap();
        arena.allocate(16).unwrap();
        arena.allocate(16).unwrap();
        assert_eq!(arena.allocation_count(), 4);
    }

    #[test]
    fn arena_auto_chunks() {
        let mut arena = ArenaAllocator::new(32);
        // These 3 allocations need 24 bytes each (aligned to 8) = 72 bytes > 32
        arena.allocate(20).unwrap();
        arena.allocate(20).unwrap();
        assert!(arena.chunk_count() >= 2);
    }

    #[test]
    fn arena_free_all() {
        let mut arena = ArenaAllocator::new(128);
        arena.allocate(16).unwrap();
        arena.allocate(32).unwrap();
        arena.free_all();
        assert_eq!(arena.chunk_count(), 1);
    }

    #[test]
    fn arena_total_allocated_bytes() {
        let mut arena = ArenaAllocator::new(128);
        arena.allocate(10).unwrap(); // aligned to 16
        arena.allocate(20).unwrap(); // aligned to 24
        assert!(arena.total_allocated_bytes() >= 30);
    }

    // -- Memory Layout Planner tests --

    #[test]
    fn layout_planner_basic() {
        let regions = vec![
            LayoutRegion { name: "header".into(), size: 16, alignment: 8, category: "meta".into() },
            LayoutRegion { name: "data".into(), size: 64, alignment: 16, category: "data".into() },
        ];
        let layout = MemoryLayoutPlanner::plan(regions);
        assert_eq!(layout.regions.len(), 2);
        assert!(layout.total_size > 0);
    }

    #[test]
    fn layout_planner_sorts_by_alignment() {
        let regions = vec![
            LayoutRegion { name: "small_align".into(), size: 8, alignment: 4, category: "cat1".into() },
            LayoutRegion { name: "big_align".into(), size: 8, alignment: 16, category: "cat1".into() },
        ];
        let layout = MemoryLayoutPlanner::plan(regions);
        // big_align should come first within its category
        assert_eq!(layout.regions[0].name, "big_align");
    }

    #[test]
    fn layout_planner_efficiency() {
        let regions = vec![
            LayoutRegion { name: "a".into(), size: 16, alignment: 16, category: "x".into() },
            LayoutRegion { name: "b".into(), size: 16, alignment: 16, category: "x".into() },
        ];
        let layout = MemoryLayoutPlanner::plan(regions);
        let eff = MemoryLayoutPlanner::efficiency(&layout);
        assert!(eff > 50.0);
        assert!(eff <= 100.0);
    }

    #[test]
    fn layout_planner_padding() {
        // big_align comes first (alignment 4), then small needs no alignment padding
        // So put them in different categories to force ordering
        let regions = vec![
            LayoutRegion { name: "first".into(), size: 1, alignment: 4, category: "c1".into() },
            LayoutRegion { name: "second".into(), size: 4, alignment: 4, category: "c2".into() },
        ];
        let layout = MemoryLayoutPlanner::plan(regions);
        // first region: 1 byte, aligned to 4 → 3 bytes padding before
        let first = &layout.regions[0];
        assert_eq!(first.padding_before, 0); // first region starts at offset 0
        // total size should be at least 1 + padding(3) + 4 = 8
        assert!(layout.total_size >= 8);
        let padding = MemoryLayoutPlanner::total_padding(&layout);
        assert!(padding > 0);
    }

    // -- Memory Safety Checker tests --

    #[test]
    fn safety_normal_usage() {
        let mut checker = MemorySafetyChecker::new();
        let addr = checker.alloc(64);
        assert!(checker.read(addr).is_ok());
        assert!(checker.write(addr, 8).is_ok());
        assert!(checker.free(addr).is_ok());
    }

    #[test]
    fn safety_detects_use_after_free() {
        let mut checker = MemorySafetyChecker::new();
        let addr = checker.alloc(64);
        checker.free(addr).unwrap();
        let result = checker.read(addr);
        assert!(matches!(result, Err(SafetyViolation::UseAfterFree { .. })));
    }

    #[test]
    fn safety_detects_double_free() {
        let mut checker = MemorySafetyChecker::new();
        let addr = checker.alloc(64);
        checker.free(addr).unwrap();
        let result = checker.free(addr);
        assert!(matches!(result, Err(SafetyViolation::DoubleFree { .. })));
    }

    #[test]
    fn safety_detects_buffer_overflow() {
        let mut checker = MemorySafetyChecker::new();
        let addr = checker.alloc(16);
        let result = checker.write(addr, 32); // write 32 bytes into 16-byte buffer
        assert!(matches!(result, Err(SafetyViolation::BufferOverflow { .. })));
    }

    #[test]
    fn safety_detects_leaks() {
        let mut checker = MemorySafetyChecker::new();
        checker.alloc(64);
        checker.alloc(128);
        checker.alloc(256);
        let leaks = checker.detect_leaks();
        assert_eq!(leaks.len(), 3);
    }

    #[test]
    fn safety_no_leaks_after_free() {
        let mut checker = MemorySafetyChecker::new();
        let a = checker.alloc(64);
        let b = checker.alloc(128);
        checker.free(a).unwrap();
        checker.free(b).unwrap();
        let leaks = checker.detect_leaks();
        assert!(leaks.is_empty());
    }

    #[test]
    fn safety_reset() {
        let mut checker = MemorySafetyChecker::new();
        checker.alloc(64);
        checker.alloc(128);
        checker.reset();
        assert!(checker.detect_leaks().is_empty());
    }

    // -- Memory Usage Tracker tests --

    #[test]
    fn tracker_records_alloc_and_free() {
        let mut tracker = MemoryUsageTracker::new();
        tracker.record_alloc("heap", 1024);
        tracker.record_alloc("heap", 512);
        tracker.record_free("heap", 512);
        assert_eq!(tracker.category_usage("heap"), 1024);
    }

    #[test]
    fn tracker_multiple_categories() {
        let mut tracker = MemoryUsageTracker::new();
        tracker.record_alloc("heap", 1024);
        tracker.record_alloc("stack", 256);
        tracker.record_alloc("arena", 2048);
        assert_eq!(tracker.category_count(), 3);
        let report = tracker.report();
        assert_eq!(report.records.len(), 3);
        assert_eq!(report.total_current, 1024 + 256 + 2048);
    }

    #[test]
    fn tracker_peak_tracking() {
        let mut tracker = MemoryUsageTracker::new();
        tracker.record_alloc("heap", 1024);
        tracker.record_alloc("heap", 1024);
        tracker.record_free("heap", 1024);
        assert_eq!(tracker.category_peak("heap"), 2048);
    }

    #[test]
    fn tracker_report_sorted_by_usage() {
        let mut tracker = MemoryUsageTracker::new();
        tracker.record_alloc("small", 64);
        tracker.record_alloc("large", 4096);
        let report = tracker.report();
        assert_eq!(report.records[0].category, "large");
        assert_eq!(report.records[1].category, "small");
    }

    #[test]
    fn tracker_reset() {
        let mut tracker = MemoryUsageTracker::new();
        tracker.record_alloc("x", 100);
        tracker.reset();
        assert_eq!(tracker.category_count(), 0);
        let report = tracker.report();
        assert!(report.records.is_empty());
    }

    #[test]
    fn tracker_allocation_counts() {
        let mut tracker = MemoryUsageTracker::new();
        tracker.record_alloc("a", 10);
        tracker.record_alloc("a", 20);
        tracker.record_free("a", 10);
        let report = tracker.report();
        let rec = report.records.iter().find(|r| r.category == "a").unwrap();
        assert_eq!(rec.allocation_count, 2);
        assert_eq!(rec.deallocation_count, 1);
    }

    #[test]
    fn test_diff_no_changes() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        let snap = s.snapshot("s1");
        let (added, removed) = s.diff(&snap);
        assert!(added.is_empty());
        assert!(removed.is_empty());
    }

    #[test]
    fn test_search_skips_expired() {
        let mut s = Store::new();
        s.put("user:1", "alice", 0, false);
        s.put("user:2", "bob", 1, false);
        thread::sleep(Duration::from_secs(2));
        let results = s.search("user:");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "user:1");
    }

    #[test]
    fn test_gc_no_expired() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 60, false);
        let removed = s.gc();
        assert_eq!(removed, 0);
        assert_eq!(s.count(), 2);
    }

    #[test]
    fn test_snapshot_excludes_expired() {
        let mut s = Store::new();
        s.put("perm", "val", 0, false);
        s.put("temp", "gone", 1, false);
        thread::sleep(Duration::from_secs(2));
        let snap = s.snapshot("check");
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].0, "perm");
    }

    #[test]
    fn test_restore_sets_ttl_zero() {
        let mut s = Store::new();
        s.put("a", "1", 1, false);
        thread::sleep(Duration::from_millis(50));
        let snap = s.snapshot("pre-restore");
        s.restore(&snap);
        assert_eq!(s.get("a"), Some("1".to_string()));
    }

    #[test]
    fn test_update_expired_fails() {
        let mut s = Store::new();
        s.put("a", "1", 1, false);
        thread::sleep(Duration::from_secs(2));
        assert!(!s.update("a", "2"));
    }

    #[test]
    fn test_put_readonly_flag() {
        let mut s = Store::new();
        s.put("a", "1", 0, true);
        let entry = &s.entries.get("a").unwrap().entry;
        assert!(entry.read_only);
    }

    #[test]
    fn test_overwrite_increments_version() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("a", "2", 0, true); // overwrite with read_only
        assert_eq!(s.get("a"), Some("2".to_string()));
        let entry = &s.entries.get("a").unwrap().entry;
        assert_eq!(entry.version, 2);
        assert!(entry.read_only);
    }

    #[test]
    fn test_empty_store_snapshot() {
        let s = Store::new();
        let snap = s.snapshot("empty");
        assert!(snap.entries.is_empty());
        assert_eq!(snap.label, "empty");
    }

    #[test]
    fn test_restore_from_empty_snapshot() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        let snap = Snapshot {
            entries: vec![],
            label: "empty".to_string(),
        };
        s.restore(&snap);
        assert_eq!(s.count(), 0);
    }
}
