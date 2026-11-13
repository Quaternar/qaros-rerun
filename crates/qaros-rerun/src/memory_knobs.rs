//! Diagnostic GPU memory knobs, read from the environment once at start (ADR-0057).
//!
//! On Windows a viewer frame that needs a host-visible staging allocation while gpu-allocator's
//! host-visible blocks are full gets a new 128 MiB block, and the block is freed again at the
//! next device poll; WDDM then builds and tears down its page tables for every eye frame. The
//! eye path therefore keeps its output uniforms and composite targets across frames;
//! `QAROS_RERUN_EYE_REUSE=0` creates them every frame again, to compare against.
//! `QAROS_RERUN_MEMORY_BLOCK_MIB` sizes gpu-allocator's device blocks (host blocks are half of
//! it) instead of wgpu's defaults. Neither is a user setting.

use std::fmt;

pub const EYE_REUSE_VARIABLE: &str = "QAROS_RERUN_EYE_REUSE";
pub const MEMORY_BLOCK_VARIABLE: &str = "QAROS_RERUN_MEMORY_BLOCK_MIB";

/// The range wgpu-hal clamps a manual block size to.
const MEMORY_BLOCK_MEBIBYTES: std::ops::RangeInclusive<u64> = 4..=256;
const MEBIBYTE: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryKnobs {
    /// Reuse the eye output's uniform buffers and composite targets across frames; off only
    /// with `QAROS_RERUN_EYE_REUSE=0`.
    pub reuse_eye_resources: bool,
    /// gpu-allocator's device block size; absent keeps wgpu's `MemoryHints::Performance`.
    pub memory_block_mebibytes: Option<u64>,
}

impl Default for MemoryKnobs {
    fn default() -> Self {
        Self {
            reuse_eye_resources: true,
            memory_block_mebibytes: None,
        }
    }
}

impl MemoryKnobs {
    pub fn from_environment() -> Result<Self, String> {
        Self::from_lookup(|variable_name| match std::env::var(variable_name) {
            Ok(variable_value) => Ok(Some(variable_value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{variable_name} is not valid UTF-8"))
            }
        })
    }

    fn from_lookup(
        lookup: impl Fn(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let reuse_eye_resources = match lookup(EYE_REUSE_VARIABLE)?.as_deref().map(str::trim) {
            None | Some("" | "1") => true,
            Some("0") => false,
            Some(other) => {
                return Err(format!("{EYE_REUSE_VARIABLE}={other}: expected 0 or 1"));
            }
        };
        let memory_block_mebibytes = match lookup(MEMORY_BLOCK_VARIABLE)?.as_deref().map(str::trim)
        {
            None | Some("") => None,
            Some(variable_value) => Some(parse_block_mebibytes(variable_value)?),
        };
        Ok(Self {
            reuse_eye_resources,
            memory_block_mebibytes,
        })
    }
}

/// Memory hints with gpu-allocator's device blocks fixed at `mebibytes`.
pub fn memory_hints_with_blocks_of(mebibytes: u64) -> wgpu::MemoryHints {
    let block_size = mebibytes * MEBIBYTE;
    wgpu::MemoryHints::Manual {
        suballocated_device_memory_block_size: block_size..block_size,
    }
}

fn parse_block_mebibytes(variable_value: &str) -> Result<u64, String> {
    let mebibytes: u64 = variable_value.parse().map_err(|_| {
        format!("{MEMORY_BLOCK_VARIABLE}={variable_value}: expected a whole number of MiB")
    })?;
    if MEMORY_BLOCK_MEBIBYTES.contains(&mebibytes) {
        Ok(mebibytes)
    } else {
        Err(format!(
            "{MEMORY_BLOCK_VARIABLE}={variable_value}: expected {} to {} MiB",
            MEMORY_BLOCK_MEBIBYTES.start(),
            MEMORY_BLOCK_MEBIBYTES.end()
        ))
    }
}

/// The startup line, every knob with its state.
impl fmt::Display for MemoryKnobs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reuse = if self.reuse_eye_resources {
            "on"
        } else {
            "off (diagnostic opt-out)"
        };
        write!(formatter, "memory knobs: {EYE_REUSE_VARIABLE} {reuse}, ")?;
        match self.memory_block_mebibytes {
            Some(mebibytes) => write!(
                formatter,
                "{MEMORY_BLOCK_VARIABLE} {mebibytes} (device blocks {mebibytes} MiB, host blocks {} MiB)",
                (mebibytes / 2).max(*MEMORY_BLOCK_MEBIBYTES.start())
            ),
            None => write!(
                formatter,
                "{MEMORY_BLOCK_VARIABLE} unset (wgpu Performance hints: device blocks 128 then 256 MiB, host blocks 64 then 128 MiB)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn knobs(variables: &[(&str, &str)]) -> Result<MemoryKnobs, String> {
        let variables: HashMap<String, String> = variables
            .iter()
            .map(|(variable_name, variable_value)| {
                ((*variable_name).to_owned(), (*variable_value).to_owned())
            })
            .collect();
        MemoryKnobs::from_lookup(|variable_name| Ok(variables.get(variable_name).cloned()))
    }

    #[test]
    fn the_block_size_bounds_are_those_wgpu_hal_accepts() {
        for refused in ["3", "257"] {
            assert_eq!(
                knobs(&[(MEMORY_BLOCK_VARIABLE, refused)]),
                Err(format!(
                    "QAROS_RERUN_MEMORY_BLOCK_MIB={refused}: expected 4 to 256 MiB"
                ))
            );
        }
        let smallest = knobs(&[(MEMORY_BLOCK_VARIABLE, "4")]).unwrap();
        assert_eq!(smallest.memory_block_mebibytes, Some(4));
        assert!(
            smallest.to_string().ends_with(
                "QAROS_RERUN_MEMORY_BLOCK_MIB 4 (device blocks 4 MiB, host blocks 4 MiB)"
            ),
            "wgpu-hal clamps the host half to 4 MiB: {smallest}"
        );
    }

    #[test]
    fn a_variable_that_cannot_be_read_stops_the_start() {
        let unreadable = MemoryKnobs::from_lookup(|variable_name| {
            Err(format!("{variable_name} is not valid UTF-8"))
        });
        assert_eq!(
            unreadable,
            Err("QAROS_RERUN_EYE_REUSE is not valid UTF-8".to_owned())
        );
    }

    #[test]
    fn reuse_is_on_and_the_block_size_unset_without_variables() {
        let knobs = knobs(&[]).unwrap();
        assert_eq!(
            knobs,
            MemoryKnobs {
                reuse_eye_resources: true,
                memory_block_mebibytes: None,
            }
        );
        assert_eq!(knobs, MemoryKnobs::default());
        assert_eq!(
            knobs.to_string(),
            "memory knobs: QAROS_RERUN_EYE_REUSE on, QAROS_RERUN_MEMORY_BLOCK_MIB unset \
             (wgpu Performance hints: device blocks 128 then 256 MiB, host blocks 64 then 128 MiB)"
        );
    }

    #[test]
    fn reuse_and_block_size_are_read_and_described() {
        let knobs = knobs(&[(EYE_REUSE_VARIABLE, "1"), (MEMORY_BLOCK_VARIABLE, "256")]).unwrap();
        assert!(knobs.reuse_eye_resources);
        assert_eq!(knobs.memory_block_mebibytes, Some(256));
        let wgpu::MemoryHints::Manual {
            suballocated_device_memory_block_size,
        } = memory_hints_with_blocks_of(256)
        else {
            panic!("manual hints expected");
        };
        assert_eq!(
            suballocated_device_memory_block_size,
            256 * MEBIBYTE..256 * MEBIBYTE
        );
        assert_eq!(
            knobs.to_string(),
            "memory knobs: QAROS_RERUN_EYE_REUSE on, QAROS_RERUN_MEMORY_BLOCK_MIB 256 \
             (device blocks 256 MiB, host blocks 128 MiB)"
        );
    }

    #[test]
    fn zero_opts_out_of_reuse_and_empty_values_keep_the_defaults() {
        let opted_out = knobs(&[(EYE_REUSE_VARIABLE, "0")]).unwrap();
        assert!(!opted_out.reuse_eye_resources);
        assert!(
            opted_out
                .to_string()
                .starts_with("memory knobs: QAROS_RERUN_EYE_REUSE off (diagnostic opt-out), "),
            "{opted_out}"
        );
        let empty = knobs(&[(EYE_REUSE_VARIABLE, ""), (MEMORY_BLOCK_VARIABLE, "")]).unwrap();
        assert_eq!(empty, MemoryKnobs::default());
    }

    #[test]
    fn values_outside_the_accepted_set_are_refused() {
        assert_eq!(
            knobs(&[(EYE_REUSE_VARIABLE, "yes")]),
            Err("QAROS_RERUN_EYE_REUSE=yes: expected 0 or 1".to_owned())
        );
        assert_eq!(
            knobs(&[(MEMORY_BLOCK_VARIABLE, "512")]),
            Err("QAROS_RERUN_MEMORY_BLOCK_MIB=512: expected 4 to 256 MiB".to_owned())
        );
        assert_eq!(
            knobs(&[(MEMORY_BLOCK_VARIABLE, "big")]),
            Err("QAROS_RERUN_MEMORY_BLOCK_MIB=big: expected a whole number of MiB".to_owned())
        );
    }
}
