//! The Vulkan memory budget of the viewer's device in the log, at start and every few seconds,
//! with wgpu's own memory blocks: on Windows, WDDM evicting the viewer's allocations while video
//! memory is free points at a per-process budget below what the viewer uses, and a block that
//! comes and goes between lines points at gpu-allocator creating and freeing a block per frame.
//!
//! The start line is at INFO, the periodic ones at DEBUG
//! (`RUST_LOG=info,qaros_rerun::memory_budget_log=debug` shows them). While DEBUG is off for
//! this module, the periodic budget query and allocator report are not taken at all.

use std::fmt;
use std::time::{Duration, Instant};

use vulkan_device::{MemoryHeapBudget, MemoryHeapBudgets, memory_heap_budgets};

pub const MEMORY_BUDGET_LOG_INTERVAL: Duration = Duration::from_secs(5);

type HeapBudgetQuery = Box<dyn FnMut() -> Result<Vec<MemoryHeapBudget>, String>>;
type AllocatorReportQuery = Box<dyn FnMut() -> Option<wgpu::AllocatorReport>>;

pub struct MemoryBudgetLog {
    query: HeapBudgetQuery,
    allocator_report: AllocatorReportQuery,
    interval: Duration,
    last_logged: Option<Instant>,
    is_unavailable: bool,
}

impl MemoryBudgetLog {
    pub fn for_device(device: wgpu::Device) -> Self {
        let report_device = device.clone();
        Self::new(
            Box::new(move || memory_heap_budgets(&device)),
            Box::new(move || report_device.generate_allocator_report()),
            MEMORY_BUDGET_LOG_INTERVAL,
        )
    }

    fn new(
        query: HeapBudgetQuery,
        allocator_report: AllocatorReportQuery,
        interval: Duration,
    ) -> Self {
        Self {
            query,
            allocator_report,
            interval,
            last_logged: None,
            is_unavailable: false,
        }
    }

    pub fn log_if_due(&mut self) {
        let is_start_line = self.is_at_start();
        let periodic_lines_shown = tracing::enabled!(tracing::Level::DEBUG);
        if let Some(line) = self.line_if_due(Instant::now(), periodic_lines_shown) {
            if is_start_line {
                re_log::info!("{line}");
            } else {
                re_log::debug!("{line}");
            }
        }
    }

    /// True until the first line is given: that one is logged at INFO, the rest at DEBUG.
    fn is_at_start(&self) -> bool {
        self.last_logged.is_none()
    }

    /// The first call and every call an interval after the last line give a line; once the
    /// budget is unavailable, that is said once and the query is not tried again, while wgpu's
    /// blocks are still logged. Without `periodic_lines_shown` only the first line is given, and
    /// neither the budget nor the allocator report is taken after it.
    fn line_if_due(&mut self, now: Instant, periodic_lines_shown: bool) -> Option<String> {
        if !self.is_at_start() && !periodic_lines_shown {
            return None;
        }
        let is_due = self
            .last_logged
            .is_none_or(|last| now.duration_since(last) >= self.interval);
        if !is_due {
            return None;
        }
        self.last_logged = Some(now);
        let report = (self.allocator_report)();
        if self.is_unavailable {
            return report.map(|report| format!("vulkan memory: {}", AllocatorBlocks(&report)));
        }
        let blocks = report
            .map(|report| format!(" | {}", AllocatorBlocks(&report)))
            .unwrap_or_default();
        match (self.query)() {
            Ok(heaps) => Some(format!(
                "vulkan memory: {}{blocks}",
                MemoryHeapBudgets(&heaps)
            )),
            Err(reason) => {
                self.is_unavailable = true;
                Some(format!(
                    "vulkan memory: budget unavailable: {reason}{blocks}"
                ))
            }
        }
    }
}

/// gpu-allocator's blocks, grouped by memory type in the device's type order, with the bytes
/// allocated in each, e.g. `wgpu blocks (MiB size/used) 256/180.50 64/64 128/0.01, reserved
/// 448 MiB, allocated 244.51 MiB`: a full block is one whose used equals its size.
struct AllocatorBlocks<'report>(&'report wgpu::AllocatorReport);

impl AllocatorBlocks<'_> {
    fn used_in(&self, block: &wgpu::wgt::MemoryBlockReport) -> u64 {
        self.0
            .allocations
            .get(block.allocations.clone())
            .unwrap_or_default()
            .iter()
            .map(|allocation| allocation.size)
            .sum()
    }
}

impl fmt::Display for AllocatorBlocks<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.0;
        if report.blocks.is_empty() {
            formatter.write_str("wgpu blocks none")?;
        } else {
            formatter.write_str("wgpu blocks (MiB size/used)")?;
            for block in &report.blocks {
                write!(
                    formatter,
                    " {}/{}",
                    Mebibytes(block.size),
                    Mebibytes(self.used_in(block))
                )?;
            }
        }
        write!(
            formatter,
            ", reserved {} MiB, allocated {} MiB",
            Mebibytes(report.total_reserved_bytes),
            Mebibytes(report.total_allocated_bytes)
        )
    }
}

/// Whole mebibytes as integers, anything else with two decimals.
struct Mebibytes(u64);

impl fmt::Display for Mebibytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MEBIBYTE: u64 = 1024 * 1024;
        if self.0.is_multiple_of(MEBIBYTE) {
            write!(formatter, "{}", self.0 / MEBIBYTE)
        } else {
            #[allow(clippy::cast_precision_loss)]
            let mebibytes = self.0 as f64 / MEBIBYTE as f64;
            write!(formatter, "{mebibytes:.2}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use vulkan_device::vk;

    use super::*;

    const GIBIBYTE: vk::DeviceSize = 1024 * 1024 * 1024;
    const MEBIBYTE: u64 = 1024 * 1024;

    fn one_device_local_heap() -> Vec<MemoryHeapBudget> {
        vec![MemoryHeapBudget {
            size: 8 * GIBIBYTE,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
            budget: 7 * GIBIBYTE,
            usage: GIBIBYTE,
        }]
    }

    /// One allocation of `used` bytes in each block of `size` bytes.
    fn report(sizes_and_used: &[(u64, u64)]) -> wgpu::AllocatorReport {
        wgpu::AllocatorReport {
            allocations: sizes_and_used
                .iter()
                .map(|&(_, used)| wgpu::wgt::AllocationReport {
                    name: "test allocation".to_owned(),
                    offset: 0,
                    size: used,
                })
                .collect(),
            blocks: sizes_and_used
                .iter()
                .enumerate()
                .map(|(index, &(size, _))| wgpu::wgt::MemoryBlockReport {
                    size,
                    allocations: index..index + 1,
                })
                .collect(),
            total_allocated_bytes: sizes_and_used.iter().map(|&(_, used)| used).sum(),
            total_reserved_bytes: sizes_and_used.iter().map(|&(size, _)| size).sum(),
        }
    }

    #[test]
    fn logs_at_once_and_then_every_interval() {
        let mut log = MemoryBudgetLog::new(
            Box::new(|| Ok(one_device_local_heap())),
            Box::new(|| None),
            Duration::from_secs(5),
        );
        let start = Instant::now();
        let expected = "vulkan memory: heap0 device-local 8.00 GiB budget 7.00 GiB usage 1.00 GiB";
        assert!(log.is_at_start());
        assert_eq!(log.line_if_due(start, true).as_deref(), Some(expected));
        assert!(!log.is_at_start(), "later lines go to DEBUG");
        assert_eq!(log.line_if_due(start + Duration::from_secs(4), true), None);
        assert_eq!(
            log.line_if_due(start + Duration::from_secs(5), true)
                .as_deref(),
            Some(expected)
        );
        assert_eq!(log.line_if_due(start + Duration::from_secs(9), true), None);
    }

    #[test]
    fn without_debug_only_the_start_line_queries_anything() {
        let queries = Rc::new(Cell::new(0));
        let reports = Rc::new(Cell::new(0));
        let counted_queries = Rc::clone(&queries);
        let counted_reports = Rc::clone(&reports);
        let mut log = MemoryBudgetLog::new(
            Box::new(move || {
                counted_queries.set(counted_queries.get() + 1);
                Ok(one_device_local_heap())
            }),
            Box::new(move || {
                counted_reports.set(counted_reports.get() + 1);
                None
            }),
            Duration::from_secs(5),
        );
        let start = Instant::now();
        assert_eq!(
            log.line_if_due(start, false).as_deref(),
            Some("vulkan memory: heap0 device-local 8.00 GiB budget 7.00 GiB usage 1.00 GiB"),
            "the start line is given without DEBUG"
        );
        for seconds in [5, 10, 60] {
            assert_eq!(
                log.line_if_due(start + Duration::from_secs(seconds), false),
                None
            );
        }
        assert_eq!(
            (queries.get(), reports.get()),
            (1, 1),
            "nothing is queried after the start line"
        );
        assert!(
            log.line_if_due(start + Duration::from_secs(61), true)
                .is_some(),
            "a line is due as soon as DEBUG is on"
        );
        assert_eq!((queries.get(), reports.get()), (2, 2));
    }

    #[test]
    fn wgpu_blocks_follow_the_heaps_and_show_a_block_that_came_and_went() {
        let reports = Rc::new(Cell::new(0));
        let counted = Rc::clone(&reports);
        let mut log = MemoryBudgetLog::new(
            Box::new(|| Ok(one_device_local_heap())),
            Box::new(move || {
                counted.set(counted.get() + 1);
                let host_block_for_staging = counted.get() == 2;
                let mut blocks = vec![
                    (256 * MEBIBYTE, 180 * MEBIBYTE + 512 * 1024),
                    (64 * MEBIBYTE, 64 * MEBIBYTE),
                ];
                if host_block_for_staging {
                    blocks.push((128 * MEBIBYTE, 64));
                }
                Some(report(&blocks))
            }),
            Duration::from_secs(5),
        );
        let start = Instant::now();
        let heaps = "heap0 device-local 8.00 GiB budget 7.00 GiB usage 1.00 GiB";
        assert_eq!(
            log.line_if_due(start, true).as_deref(),
            Some(
                format!(
                    "vulkan memory: {heaps} | wgpu blocks (MiB size/used) 256/180.50 64/64, \
                     reserved 320 MiB, allocated 244.50 MiB"
                )
                .as_str()
            )
        );
        assert_eq!(
            log.line_if_due(start + Duration::from_secs(5), true)
                .as_deref(),
            Some(
                format!(
                    "vulkan memory: {heaps} | wgpu blocks (MiB size/used) 256/180.50 64/64 \
                     128/0.00, reserved 448 MiB, allocated 244.50 MiB"
                )
                .as_str()
            )
        );
        assert_eq!(log.line_if_due(start + Duration::from_secs(6), true), None);
        assert_eq!(
            reports.get(),
            2,
            "the report is taken only when a line is due"
        );
    }

    #[test]
    fn an_unavailable_budget_is_logged_once_and_not_queried_again() {
        let queries = Rc::new(Cell::new(0));
        let counted = Rc::clone(&queries);
        let mut log = MemoryBudgetLog::new(
            Box::new(move || {
                counted.set(counted.get() + 1);
                Err("VK_EXT_memory_budget is not enabled on the device".to_owned())
            }),
            Box::new(|| None),
            Duration::from_secs(5),
        );
        let start = Instant::now();
        assert_eq!(
            log.line_if_due(start, true).as_deref(),
            Some(
                "vulkan memory: budget unavailable: VK_EXT_memory_budget is not enabled on the device"
            )
        );
        assert_eq!(log.line_if_due(start + Duration::from_secs(30), true), None);
        assert_eq!(queries.get(), 1);
    }

    #[test]
    fn without_a_budget_the_blocks_go_on_being_logged() {
        let mut log = MemoryBudgetLog::new(
            Box::new(|| Err("the device is not on Vulkan".to_owned())),
            Box::new(|| Some(report(&[]))),
            Duration::from_secs(5),
        );
        let start = Instant::now();
        assert_eq!(
            log.line_if_due(start, true).as_deref(),
            Some(
                "vulkan memory: budget unavailable: the device is not on Vulkan | wgpu blocks \
                 none, reserved 0 MiB, allocated 0 MiB"
            )
        );
        assert_eq!(
            log.line_if_due(start + Duration::from_secs(5), true)
                .as_deref(),
            Some("vulkan memory: wgpu blocks none, reserved 0 MiB, allocated 0 MiB")
        );
    }
}
