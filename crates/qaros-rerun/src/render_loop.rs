//! The render loop a viewer runs (ADR-0088): the eyes on a render thread of their own by
//! default, inside the viewer frame where that cannot run, unless a flag decides.
//!
//! The default falls back before anything is built, since the sink and the QAROS session live
//! on the thread that renders and cannot move afterwards. A QAROS Vulkan sink in a window needs
//! raw batches on a queue of their own (ADR-0081); the device is created after onboarding on
//! the session's adapter, so at start the GPU is forecast with the selector the session's
//! adapter is expected to match: `--vulkan-device`, else the guess `qaros_setup` makes for a
//! session that names no adapter. Should the session name a GPU the forecast did not see, the
//! sink refuses as for `--decoupled-render` (`qaros_setup::check_decoupled_window`).

use vulkan_device::{PhysicalDeviceSelector, RawQueue, RawQueueForecast, VulkanDevice};

use crate::device_setup::GraphicsChoice;
use crate::options::{Options, RenderLoop, SinkKind};

/// Turns the default into inline rendering, with a WARN line, where it cannot run decoupled,
/// and keeps the reason in `options.render_fallback` for the startup summary.
pub async fn settle(options: &mut Options) {
    if options.render_loop != RenderLoop::DecoupledWherePossible {
        return;
    }
    let forecast = match second_queue_needed_on(options) {
        Some(selector) => Some(GraphicsChoice::forecast_raw_queue(&selector).await),
        None => None,
    };
    if let Some(reason) = reason_to_render_inline(options, forecast.as_ref()) {
        re_log::warn!("{}", fallback_warning(&reason));
        options.render_loop = RenderLoop::Inline;
        options.render_fallback = Some(reason);
    }
}

fn fallback_warning(reason: &str) -> String {
    format!(
        "headset: the decoupled render loop cannot run, because {reason}; drawing the eyes inside \
         the viewer frame instead. --inline-render chooses that without this warning; \
         --decoupled-render refuses to start instead"
    )
}

/// Where the viewer's raw Vulkan batches go, once its device exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawBatches {
    OwnQueue(u32),
    WgpusQueue,
}

impl RawBatches {
    fn of(raw_queue: &RawQueue) -> Self {
        if raw_queue.shares_wgpu_queue() {
            Self::WgpusQueue
        } else {
            Self::OwnQueue(raw_queue.queue_index())
        }
    }
}

/// The startup summary's render line, once the sink and the viewer's device (if any) exist.
pub fn summary(options: &Options, vulkan: Option<&VulkanDevice>) -> String {
    render_line(
        options.render_loop,
        options.render_fallback.as_deref(),
        vulkan.map(|vulkan| RawBatches::of(&vulkan.raw_queue)),
    )
}

/// The loop that runs, and why when the default fell back.
fn render_line(
    render_loop: RenderLoop,
    fallback_reason: Option<&str>,
    raw_batches: Option<RawBatches>,
) -> String {
    if !render_loop.is_decoupled() {
        return match fallback_reason {
            Some(reason) => format!("render: inline, because {reason}"),
            None => "render: inline (--inline-render)".to_owned(),
        };
    }
    match raw_batches {
        Some(RawBatches::OwnQueue(queue_index)) => {
            format!("render: decoupled (raw batches on queue {queue_index})")
        }
        Some(RawBatches::WgpusQueue) => {
            "render: decoupled (raw batches share wgpu's queue under the submission lock)"
                .to_owned()
        }
        None => "render: decoupled".to_owned(),
    }
}

/// The GPU whose queues decide, for a QAROS Vulkan sink in a window; `None` otherwise.
fn second_queue_needed_on(options: &Options) -> Option<PhysicalDeviceSelector> {
    let windowed = options.headless_size.is_none();
    (options.qaros_vulkan() && windowed).then(|| {
        options
            .vulkan_device
            .clone()
            .unwrap_or(PhysicalDeviceSelector::Discrete)
    })
}

fn reason_to_render_inline(
    options: &Options,
    forecast: Option<&Result<RawQueueForecast, String>>,
) -> Option<String> {
    if options.sink == SinkKind::Off {
        return Some("there is no sink to render for".to_owned());
    }
    match forecast? {
        Ok(forecast) if forecast.raw_batches_get_a_queue_of_their_own() => None,
        Ok(forecast) => Some(format!(
            "the QAROS Vulkan sink in a window needs a second queue in the GPU's graphics queue \
             family, and {} has {}",
            forecast.physical_device.name, forecast.wgpu_family_queue_count
        )),
        Err(error) => Some(format!(
            "the GPU's queues could not be read for the QAROS Vulkan sink in a window ({error})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;
    use vulkan_device::{PhysicalDeviceIdentity, vk};

    fn options(arguments: &[&str]) -> Options {
        Options::parse(arguments.iter().map(|argument| (*argument).to_owned()))
            .expect("valid options")
    }

    fn forecast_with(wgpu_family_queue_count: u32) -> RawQueueForecast {
        RawQueueForecast {
            physical_device: PhysicalDeviceIdentity {
                handle: vk::PhysicalDevice::null(),
                name: "Test GPU".to_owned(),
                device_type: vk::PhysicalDeviceType::DISCRETE_GPU,
                vendor_id: 0x10de,
                device_id: 1,
                api_version: vk::API_VERSION_1_3,
                device_uuid: [0; vk::UUID_SIZE],
                device_luid: None,
            },
            wgpu_family_queue_count,
        }
    }

    const QAROS_VULKAN_WINDOW: &[&str] = &[
        "--sink",
        "qaros",
        "--qaros-graphics",
        "vulkan",
        "--local-app",
        "--window",
    ];

    #[test]
    fn a_qaros_vulkan_sink_in_a_window_asks_about_the_named_device_or_the_discrete_guess() {
        assert_eq!(
            second_queue_needed_on(&options(QAROS_VULKAN_WINDOW)),
            Some(PhysicalDeviceSelector::Discrete)
        );
        let mut named = QAROS_VULKAN_WINDOW.to_vec();
        named.extend(["--vulkan-device", "name:RTX"]);
        assert_eq!(
            second_queue_needed_on(&options(&named)),
            Some(PhysicalDeviceSelector::NameContains("RTX".to_owned()))
        );
        let headless = [
            "--sink",
            "qaros",
            "--qaros-graphics",
            "vulkan",
            "--launched-by-qaros",
        ];
        assert_eq!(second_queue_needed_on(&options(&headless)), None);
        let cpu = ["--sink", "qaros", "--qaros-graphics", "cpu", "--local-app"];
        assert_eq!(second_queue_needed_on(&options(&cpu)), None);
        assert_eq!(second_queue_needed_on(&options(&[])), None);
    }

    #[test]
    fn the_default_stays_decoupled_where_the_gpu_has_a_second_queue_or_none_is_needed() {
        let window = options(QAROS_VULKAN_WINDOW);
        assert_eq!(
            reason_to_render_inline(&window, Some(&Ok(forecast_with(16)))),
            None
        );
        assert_eq!(
            reason_to_render_inline(&window, Some(&Ok(forecast_with(2)))),
            None
        );
        assert_eq!(reason_to_render_inline(&options(&[]), None), None);
    }

    #[test]
    fn the_default_falls_back_to_inline_on_one_queue_an_unread_gpu_or_no_sink() {
        let window = options(QAROS_VULKAN_WINDOW);
        assert_eq!(
            reason_to_render_inline(&window, Some(&Ok(forecast_with(1)))).as_deref(),
            Some(
                "the QAROS Vulkan sink in a window needs a second queue in the GPU's graphics \
                 queue family, and Test GPU has 1"
            )
        );
        assert_eq!(
            reason_to_render_inline(&window, Some(&Err("no Vulkan loader".to_owned()))).as_deref(),
            Some(
                "the GPU's queues could not be read for the QAROS Vulkan sink in a window (no \
                 Vulkan loader)"
            )
        );
        assert_eq!(
            reason_to_render_inline(&options(&["--sink", "off"]), None).as_deref(),
            Some("there is no sink to render for")
        );
    }

    /// The level and message of every event logged while it is the default subscriber.
    #[derive(Clone, Default)]
    struct CapturedEvents(Arc<Mutex<Vec<(tracing::Level, String)>>>);

    struct MessageField<'text>(&'text mut String);

    impl tracing::field::Visit for MessageField<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                *self.0 = format!("{value:?}");
            }
        }
    }

    impl tracing::Subscriber for CapturedEvents {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut message = String::new();
            event.record(&mut MessageField(&mut message));
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((*event.metadata().level(), message));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn settled_with_events(arguments: &[&str]) -> (Options, Vec<(tracing::Level, String)>) {
        let events = CapturedEvents::default();
        let mut options = options(arguments);
        tracing::subscriber::with_default(events.clone(), || {
            pollster::block_on(settle(&mut options));
        });
        let logged = events
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        (options, logged)
    }

    #[test]
    fn the_default_without_a_sink_falls_back_with_one_warning_naming_the_reason_and_the_flags() {
        let (options, logged) = settled_with_events(&["--sink", "off"]);
        assert_eq!(options.render_loop, RenderLoop::Inline);
        assert_eq!(
            options.render_fallback.as_deref(),
            Some("there is no sink to render for")
        );
        assert_eq!(
            logged,
            [(
                tracing::Level::WARN,
                "headset: the decoupled render loop cannot run, because there is no sink to \
                 render for; drawing the eyes inside the viewer frame instead. --inline-render \
                 chooses that without this warning; --decoupled-render refuses to start instead"
                    .to_owned()
            )]
        );
    }

    #[test]
    fn settle_leaves_a_runnable_default_and_the_flags_alone_without_a_word() {
        for (arguments, expected) in [
            (&[][..], RenderLoop::DecoupledWherePossible),
            (&["--decoupled-render"][..], RenderLoop::Decoupled),
            (&["--inline-render"][..], RenderLoop::Inline),
            (
                &["--inline-render", "--sink", "off"][..],
                RenderLoop::Inline,
            ),
        ] {
            let (options, logged) = settled_with_events(arguments);
            assert_eq!(options.render_loop, expected, "{arguments:?}");
            assert_eq!(options.render_fallback, None, "{arguments:?}");
            assert_eq!(logged, [], "{arguments:?}");
        }
    }

    #[test]
    fn the_summary_names_the_loop_and_where_raw_batches_go_or_why_it_fell_back() {
        assert_eq!(
            render_line(
                RenderLoop::DecoupledWherePossible,
                None,
                Some(RawBatches::OwnQueue(1))
            ),
            "render: decoupled (raw batches on queue 1)"
        );
        assert_eq!(
            render_line(RenderLoop::Decoupled, None, Some(RawBatches::WgpusQueue)),
            "render: decoupled (raw batches share wgpu's queue under the submission lock)"
        );
        assert_eq!(
            render_line(RenderLoop::DecoupledWherePossible, None, None),
            "render: decoupled"
        );
        assert_eq!(
            render_line(
                RenderLoop::Inline,
                Some("there is no sink to render for"),
                None
            ),
            "render: inline, because there is no sink to render for"
        );
        assert_eq!(
            render_line(RenderLoop::Inline, None, Some(RawBatches::OwnQueue(1))),
            "render: inline (--inline-render)"
        );
    }
}
