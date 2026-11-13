//! Choosing the physical device from the `VkInstance`, as QuaternAR's StereoKit fork does with
//! `qar_vulkan_device_picker_t`: called once, after the instance exists and before the device
//! is created; `None` keeps the default choice.

use std::fmt::{self, Write as _};

use ash::vk;

/// Given the instance wgpu created, returns the physical device to render with, `Ok(None)` for
/// the default choice, or an error when the wanted device is not there.
pub type PhysicalDevicePicker =
    Box<dyn FnOnce(&ash::Instance) -> Result<Option<vk::PhysicalDevice>, String> + Send>;

/// What identifies a physical device across APIs: QAROS's `QarGraphicsDeviceId` carries a LUID
/// (Windows) or a UUID, DXGI adapters a LUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalDeviceIdentity {
    pub handle: vk::PhysicalDevice,
    pub name: String,
    pub device_type: vk::PhysicalDeviceType,
    pub vendor_id: u32,
    pub device_id: u32,
    pub api_version: u32,
    pub device_uuid: [u8; vk::UUID_SIZE],
    /// Only where the driver reports `deviceLUIDValid` (Windows drivers; MoltenVK 1.4 too).
    pub device_luid: Option<[u8; vk::LUID_SIZE]>,
}

impl fmt::Display for PhysicalDeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({:?}, vendor {:#06x}, device {:#x}, Vulkan {}.{}.{}, uuid {}",
            self.name,
            self.device_type,
            self.vendor_id,
            self.device_id,
            vk::api_version_major(self.api_version),
            vk::api_version_minor(self.api_version),
            vk::api_version_patch(self.api_version),
            hex(&self.device_uuid),
        )?;
        match &self.device_luid {
            Some(luid) => write!(formatter, ", luid {})", hex(luid)),
            None => write!(formatter, ", no luid)"),
        }
    }
}

/// Every physical device of `instance` with its name, UUID and LUID
/// (`vkGetPhysicalDeviceProperties2` with `VkPhysicalDeviceIDProperties`, Vulkan 1.1).
pub fn describe_physical_devices(
    instance: &ash::Instance,
) -> Result<Vec<PhysicalDeviceIdentity>, vk::Result> {
    #[allow(unsafe_code)]
    // SAFETY: `instance` is a live instance; the handles it returns are only queried.
    let physical_devices = unsafe { instance.enumerate_physical_devices() }?;
    Ok(physical_devices
        .into_iter()
        .map(|handle| describe_physical_device(instance, handle))
        .collect())
}

fn describe_physical_device(
    instance: &ash::Instance,
    handle: vk::PhysicalDevice,
) -> PhysicalDeviceIdentity {
    let mut id_properties = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id_properties);
    #[allow(unsafe_code)]
    // SAFETY: `handle` comes from `instance`, and the chained struct outlives the call.
    unsafe {
        instance.get_physical_device_properties2(handle, &mut properties);
    }
    let core = properties.properties;
    PhysicalDeviceIdentity {
        handle,
        name: core.device_name_as_c_str().map_or_else(
            |_| "?".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
        device_type: core.device_type,
        vendor_id: core.vendor_id,
        device_id: core.device_id,
        api_version: core.api_version,
        device_uuid: id_properties.device_uuid,
        device_luid: (id_properties.device_luid_valid == vk::TRUE)
            .then_some(id_properties.device_luid),
    }
}

/// The ways to name a physical device on the command line: what the Mac can match on (name,
/// UUID) and what QAROS will give on Windows (LUID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhysicalDeviceSelector {
    /// No choice of ours: wgpu's adapter order and the caller's fallback selection.
    Default,
    /// The first device whose name contains this text, ignoring case.
    NameContains(String),
    Uuid([u8; vk::UUID_SIZE]),
    Luid([u8; vk::LUID_SIZE]),
    /// Position in `vkEnumeratePhysicalDevices` order.
    Index(usize),
    /// The first discrete NVIDIA GPU, else the first discrete GPU, else the first device: a
    /// guess for a session that names no adapter, aimed at the GPU a hybrid laptop renders
    /// headsets with.
    Discrete,
}

/// `VkPhysicalDeviceProperties::vendorID` of NVIDIA.
const NVIDIA_VENDOR_ID: u32 = 0x10de;

impl PhysicalDeviceSelector {
    /// `default`, `discrete`, `name:TEXT`, `uuid:HEX` (32 digits), `luid:HEX` (16 digits) or
    /// `index:N`.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "default" => return Ok(Self::Default),
            "discrete" => return Ok(Self::Discrete),
            _ => {}
        }
        let (kind, value) = text.split_once(':').ok_or_else(|| {
            format!("expected default, discrete, name:, uuid:, luid: or index:, got {text:?}")
        })?;
        match kind {
            "name" if !value.is_empty() => Ok(Self::NameContains(value.to_owned())),
            "uuid" => parse_hex(value).map(Self::Uuid),
            "luid" => parse_hex(value).map(Self::Luid),
            "index" => value
                .parse()
                .map(Self::Index)
                .map_err(|_| format!("index:{value} is not a number")),
            _ => Err(format!("unknown physical device selector {text:?}")),
        }
    }

    /// An adapter as hex of its id bytes, the form `qaros::GraphicsAdapter` prints: 16
    /// hexadecimal digits for a LUID, 32 for a UUID, either case.
    pub fn from_adapter_id(text: &str) -> Result<Self, String> {
        let text = text.trim();
        match text.len() {
            16 => parse_hex(text).map(Self::Luid),
            32 => parse_hex(text).map(Self::Uuid),
            _ => Err(format!(
                "adapter id {text:?} is neither a LUID (16 hexadecimal digits) nor a UUID (32)"
            )),
        }
    }

    /// The identity this selector names among `devices`, `Ok(None)` for [`Self::Default`].
    pub fn select<'devices>(
        &self,
        devices: &'devices [PhysicalDeviceIdentity],
    ) -> Result<Option<&'devices PhysicalDeviceIdentity>, String> {
        let found = match self {
            Self::Default => return Ok(None),
            Self::NameContains(text) => {
                let text = text.to_lowercase();
                devices
                    .iter()
                    .find(|device| device.name.to_lowercase().contains(&text))
            }
            Self::Uuid(uuid) => devices.iter().find(|device| &device.device_uuid == uuid),
            Self::Luid(luid) => devices
                .iter()
                .find(|device| device.device_luid.as_ref() == Some(luid)),
            Self::Index(index) => devices.get(*index),
            Self::Discrete => {
                let is_discrete = |device: &&PhysicalDeviceIdentity| {
                    device.device_type == vk::PhysicalDeviceType::DISCRETE_GPU
                };
                devices
                    .iter()
                    .filter(is_discrete)
                    .find(|device| device.vendor_id == NVIDIA_VENDOR_ID)
                    .or_else(|| devices.iter().find(is_discrete))
                    .or_else(|| devices.first())
            }
        };
        found.map(Some).ok_or_else(|| {
            let mut message = format!("no physical device matches {self:?}; the instance has:");
            for device in devices {
                let _ = write!(message, "\n    {device}");
            }
            message
        })
    }

    /// A picker that applies this selector and logs every candidate.
    pub fn into_picker(self) -> PhysicalDevicePicker {
        Box::new(move |instance| {
            let devices = describe_physical_devices(instance)
                .map_err(|error| format!("vkEnumeratePhysicalDevices: {error}"))?;
            for device in &devices {
                log::info!("vulkan-device: candidate {device}");
            }
            Ok(self.select(&devices)?.map(|device| device.handle))
        })
    }
}

fn parse_hex<const LENGTH: usize>(text: &str) -> Result<[u8; LENGTH], String> {
    let digits: String = text.chars().filter(|&digit| digit != '-').collect();
    if digits.len() != LENGTH * 2 || !digits.is_ascii() {
        return Err(format!("{text:?} is not {LENGTH} hexadecimal bytes"));
    }
    let mut bytes = [0; LENGTH];
    for (byte, pair) in bytes.iter_mut().zip(digits.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| format!("{text:?} is not hexadecimal"))?;
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, uuid_byte: u8, luid: Option<[u8; 8]>) -> PhysicalDeviceIdentity {
        PhysicalDeviceIdentity {
            handle: vk::PhysicalDevice::null(),
            name: name.to_owned(),
            device_type: vk::PhysicalDeviceType::DISCRETE_GPU,
            vendor_id: 0x10de,
            device_id: 1,
            api_version: vk::make_api_version(0, 1, 3, 0),
            device_uuid: [uuid_byte; 16],
            device_luid: luid,
        }
    }

    #[test]
    fn parses_every_selector() {
        assert_eq!(
            PhysicalDeviceSelector::parse("default"),
            Ok(PhysicalDeviceSelector::Default)
        );
        assert_eq!(
            PhysicalDeviceSelector::parse("name:Apple M5"),
            Ok(PhysicalDeviceSelector::NameContains("Apple M5".to_owned()))
        );
        assert_eq!(
            PhysicalDeviceSelector::parse("luid:0102030405060708"),
            Ok(PhysicalDeviceSelector::Luid([1, 2, 3, 4, 5, 6, 7, 8]))
        );
        assert_eq!(
            PhysicalDeviceSelector::parse("uuid:00112233-4455-6677-8899-aabbccddeeff"),
            Ok(PhysicalDeviceSelector::Uuid([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]))
        );
        assert_eq!(
            PhysicalDeviceSelector::parse("index:1"),
            Ok(PhysicalDeviceSelector::Index(1))
        );
        assert!(PhysicalDeviceSelector::parse("luid:0102").is_err());
        assert!(PhysicalDeviceSelector::parse("uuid:zz112233445566778899aabbccddeeff").is_err());
        assert!(PhysicalDeviceSelector::parse("nvidia").is_err());
        assert!(PhysicalDeviceSelector::parse("name:").is_err());
    }

    #[test]
    fn adapter_ids_are_luids_or_uuids_by_length() {
        assert_eq!(
            PhysicalDeviceSelector::from_adapter_id("000000010000074B"),
            Ok(PhysicalDeviceSelector::Luid([0, 0, 0, 1, 0, 0, 7, 0x4b]))
        );
        assert_eq!(
            PhysicalDeviceSelector::from_adapter_id("00112233445566778899aabbccddeeff\n"),
            Ok(PhysicalDeviceSelector::Uuid([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]))
        );
        assert!(PhysicalDeviceSelector::from_adapter_id("0102").is_err());
        assert!(PhysicalDeviceSelector::from_adapter_id("zz00000100000740").is_err());
        assert!(PhysicalDeviceSelector::from_adapter_id("").is_err());
    }

    #[test]
    fn selects_by_name_uuid_luid_and_index() {
        let devices = [
            identity("Intel(R) UHD Graphics", 1, Some([1; 8])),
            identity("NVIDIA GeForce RTX 4090", 2, Some([2; 8])),
        ];
        let name_of = |selector: PhysicalDeviceSelector| {
            selector
                .select(&devices)
                .unwrap()
                .map(|device| device.name.clone())
        };
        assert_eq!(
            name_of(PhysicalDeviceSelector::NameContains("geforce".to_owned())).as_deref(),
            Some("NVIDIA GeForce RTX 4090")
        );
        assert_eq!(
            name_of(PhysicalDeviceSelector::Uuid([1; 16])).as_deref(),
            Some("Intel(R) UHD Graphics")
        );
        assert_eq!(
            name_of(PhysicalDeviceSelector::Luid([2; 8])).as_deref(),
            Some("NVIDIA GeForce RTX 4090")
        );
        assert_eq!(
            name_of(PhysicalDeviceSelector::Index(0)).as_deref(),
            Some("Intel(R) UHD Graphics")
        );
        assert_eq!(name_of(PhysicalDeviceSelector::Default), None);
    }

    #[test]
    fn discrete_prefers_nvidia_then_any_discrete_then_the_first() {
        let typed = |name: &str, device_type, vendor_id| PhysicalDeviceIdentity {
            device_type,
            vendor_id,
            ..identity(name, 0, None)
        };
        let integrated = typed("Intel iGPU", vk::PhysicalDeviceType::INTEGRATED_GPU, 0x8086);
        let amd = typed("AMD dGPU", vk::PhysicalDeviceType::DISCRETE_GPU, 0x1002);
        let nvidia = typed("NVIDIA dGPU", vk::PhysicalDeviceType::DISCRETE_GPU, 0x10de);
        let name_of = |devices: &[PhysicalDeviceIdentity]| {
            PhysicalDeviceSelector::Discrete
                .select(devices)
                .unwrap()
                .map(|device| device.name.clone())
        };
        assert_eq!(
            name_of(&[integrated.clone(), amd.clone(), nvidia]).as_deref(),
            Some("NVIDIA dGPU")
        );
        assert_eq!(
            name_of(&[integrated.clone(), amd]).as_deref(),
            Some("AMD dGPU")
        );
        assert_eq!(name_of(&[integrated]).as_deref(), Some("Intel iGPU"));
        assert!(PhysicalDeviceSelector::Discrete.select(&[]).is_err());
        assert_eq!(
            PhysicalDeviceSelector::parse("discrete"),
            Ok(PhysicalDeviceSelector::Discrete)
        );
    }

    #[test]
    fn a_missing_device_is_an_error_naming_the_candidates() {
        let devices = [identity("Apple M5 Pro", 3, None)];
        let error = PhysicalDeviceSelector::Luid([9; 8])
            .select(&devices)
            .unwrap_err();
        assert!(error.contains("Apple M5 Pro"), "{error}");
        assert!(error.contains("no luid"), "{error}");
    }
}
