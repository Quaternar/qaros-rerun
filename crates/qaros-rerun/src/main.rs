//! The Rerun viewer with a headset renderer: the streamed 3D view is rendered once per eye of
//! every rendered user, inside the viewer frame, and handed to a sink with color, depth and the
//! near/far planes used. On a Mac the sink writes PNGs per simulated user; the QAROS render
//! senders (one per user) are the Windows sink, fed from the CPU or, in their Vulkan mode,
//! rendered into directly on the viewer's own Vulkan device.

mod decoupled;
mod device_setup;
mod draw_data_copy;
mod eye_output;
mod eye_render;
mod headless;
mod headset;
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
mod lan_address;
mod memory_budget_log;
mod memory_knobs;
mod options;
mod panel_capture;
mod panel_render;
#[cfg(feature = "qaros")]
mod qaros_setup;
mod viewer_app;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use frame_sink::{
    GpuReadbackSink, HeadMotion, ImageSink, ImageSinkOptions, MaxUsers, ScriptedHeadMotion,
    SimulatedUser,
};
use panel_server::ViewFrames;
use rerun::external::re_sdk_types::blueprint::components::PanelState;
use rerun::external::{eframe, re_crash_handler, re_grpc_server, re_memory, re_viewer, tokio};

use crate::decoupled::{DecoupledHeadset, RenderThreadSink, SceneCapture, SceneHandoff};
use crate::device_setup::GraphicsChoice;
use crate::headless::{HeadlessOptions, run_headless};
use crate::headset::{HeadsetRenderer, HeadsetSettings, HeadsetSink};
use crate::memory_budget_log::MemoryBudgetLog;
use crate::memory_knobs::MemoryKnobs;
use crate::options::{ImageTargets, MotionKind, Options, ParseOutcome, SinkKind, USAGE};
use crate::panel_capture::{PanelCapture, SharedViewDirectory, ViewDirectory};
use crate::viewer_app::{
    FrameHook, Headset, HeadsetViewerApp, SharedHeadset, StopRequest, end_process_after_teardown,
};

#[global_allocator]
static GLOBAL: re_memory::AccountingAllocator<mimalloc::MiMalloc> =
    re_memory::AccountingAllocator::new(mimalloc::MiMalloc);

#[derive(Default)]
struct SinkSetup {
    sink: Option<HeadsetSink>,
    frame_hooks: Vec<Box<dyn FrameHook>>,
    stop_request: Option<StopRequest>,
    /// The viewer's own Vulkan device, egui's device from here on.
    vulkan: Option<vulkan_device::VulkanDevice>,
}

type WakeViewer = Arc<dyn Fn() + Send + Sync>;

/// Wakes the viewer from library threads once the egui context exists, and with
/// `--decoupled-render` also the render thread, which polls the sink.
fn wake_viewer_handle(
    egui_context: &Arc<OnceLock<egui::Context>>,
    render_thread: Option<&Arc<SceneHandoff>>,
) -> WakeViewer {
    let egui_context = Arc::clone(egui_context);
    let render_thread = render_thread.map(Arc::clone);
    Arc::new(move || {
        if let Some(context) = egui_context.get() {
            context.request_repaint();
        }
        if let Some(handoff) = &render_thread {
            handoff.wake();
        }
    })
}

/// A `--vulkan-device` device is created first (and the QAROS session joined on its adapter);
/// without one, the QAROS Vulkan sink creates the device after onboarding, on the session's
/// adapter.
async fn build_sink(
    options: &Options,
    graphics: &GraphicsChoice,
    wake_viewer: WakeViewer,
    panel_frames: Option<ViewFrames>,
) -> Result<SinkSetup, String> {
    let named_vulkan = graphics.create_named_vulkan_device().await?;
    match options.sink {
        SinkKind::Off => Ok(SinkSetup {
            vulkan: named_vulkan,
            ..SinkSetup::default()
        }),
        #[cfg(feature = "qaros")]
        SinkKind::Qaros => {
            let setup =
                qaros_setup::build(options, graphics, wake_viewer, panel_frames, named_vulkan)
                    .await?;
            Ok(SinkSetup {
                sink: Some(setup.sink),
                frame_hooks: setup.frame_hooks,
                stop_request: Some(setup.stop_request),
                vulkan: setup.vulkan,
            })
        }
        #[cfg(not(feature = "qaros"))]
        SinkKind::Qaros => {
            let _ = (graphics, wake_viewer, panel_frames, named_vulkan);
            Err("this build has no QAROS sink; rebuild with the default `qaros` feature".to_owned())
        }
        SinkKind::Images => {
            let sink = ImageSink::new(
                ImageSinkOptions {
                    output_directory: options.output_directory.clone(),
                    resolution_in_pixel: options.eye_resolution,
                    depth_range: options.depth_range,
                    write_every_nth_frame: options.write_every_nth_frame,
                    frames_per_user: options.stop_after_frames.unwrap_or(120),
                    write_raw_depth: options.write_raw_depth,
                },
                simulated_users(options),
                Box::new(MaxUsers {
                    maximum: options.max_users,
                }),
            )
            .map_err(|error| error.to_string())?;
            let sink = match (options.image_targets, named_vulkan.as_ref()) {
                (ImageTargets::Cpu, _) => HeadsetSink::Cpu(Box::new(sink)),
                (ImageTargets::Gpu, Some(vulkan)) => HeadsetSink::Gpu(Box::new(
                    GpuReadbackSink::new(&vulkan.device, &vulkan.queue, sink)
                        .with_depth_scale_down(options.depth_scale_down),
                )),
                (ImageTargets::Gpu, None) => {
                    return Err("--image-targets gpu without the viewer's own device".to_owned());
                }
            };
            Ok(SinkSetup {
                sink: Some(sink),
                vulkan: named_vulkan,
                ..SinkSetup::default()
            })
        }
    }
}

/// `--users` heads around the world origin, spread evenly over the circle, joining
/// `--join-interval` viewer frames apart: walking the circle or standing on it.
fn simulated_users(options: &Options) -> Vec<SimulatedUser> {
    let center = options.world_origin;
    let user_count = options.simulated_users.max(1);
    (0..user_count)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let start_angle = std::f32::consts::TAU * index as f32 / user_count as f32;
            let motion = match options.motion {
                MotionKind::Circle => HeadMotion::CircleAround {
                    center,
                    radius: options.circle_radius,
                    eye_height: 1.6,
                    period_seconds: 4.0,
                    start_angle,
                },
                MotionKind::Still => HeadMotion::Still {
                    position: glam::Vec3::new(
                        center.x + options.circle_radius * start_angle.sin(),
                        1.6,
                        center.z + options.circle_radius * start_angle.cos(),
                    ),
                    yaw: start_angle,
                },
            };
            SimulatedUser {
                label: format!("simulated head {}", index + 1),
                joins_at_tick: u64::from(index) * options.join_interval_in_viewer_frames,
                pose_source: Box::new(ScriptedHeadMotion::new(motion)),
            }
        })
        .collect()
}

fn headset_settings(options: &Options, memory_knobs: &MemoryKnobs) -> HeadsetSettings {
    HeadsetSettings {
        placement: options.placement(),
        world_coordinates_override: options.world_coordinates_override,
        transparent_background: !options.opaque_background,
        frame_stamp: options.frame_stamp,
        reuse_eye_resources: memory_knobs.reuse_eye_resources,
        ..HeadsetSettings::default()
    }
}

fn headset_for(options: &Options, memory_knobs: &MemoryKnobs, sink: HeadsetSink) -> Headset {
    let settings = headset_settings(options, memory_knobs);
    Headset {
        renderer: Arc::new(Mutex::new(HeadsetRenderer::new(settings, &sink))),
        sink,
    }
}

/// The sink, the panel hooks, the stop request and the viewer's own Vulkan device, built on
/// the UI thread, or with `--decoupled-render` on the render thread, which keeps the sink and
/// the hooks.
struct ViewerSinks {
    headset: Option<Headset>,
    decoupled: Option<DecoupledHeadset>,
    frame_hooks: Vec<Box<dyn FrameHook>>,
    stop_request: Option<StopRequest>,
    vulkan: Option<vulkan_device::VulkanDevice>,
}

async fn build_viewer_sinks(
    options: &Options,
    memory_knobs: &MemoryKnobs,
    graphics: &GraphicsChoice,
    egui_context: &Arc<OnceLock<egui::Context>>,
    panel_frames: Option<ViewFrames>,
) -> Result<ViewerSinks, String> {
    if !options.decoupled_render {
        let SinkSetup {
            sink,
            frame_hooks,
            stop_request,
            vulkan,
        } = build_sink(
            options,
            graphics,
            wake_viewer_handle(egui_context, None),
            panel_frames,
        )
        .await?;
        return Ok(ViewerSinks {
            headset: sink.map(|sink| headset_for(options, memory_knobs, sink)),
            decoupled: None,
            frame_hooks,
            stop_request,
            vulkan,
        });
    }
    let handoff = Arc::new(SceneHandoff::default());
    let wake_viewer = wake_viewer_handle(egui_context, Some(&handoff));
    let runtime = tokio::runtime::Handle::current();
    let thread_options = options.clone();
    let thread_graphics = graphics.clone();
    let (decoupled, (stop_request, vulkan)) = DecoupledHeadset::start(
        headset_settings(options, memory_knobs),
        handoff,
        move || {
            let SinkSetup {
                sink,
                frame_hooks,
                stop_request,
                vulkan,
            } = runtime.block_on(build_sink(
                &thread_options,
                &thread_graphics,
                wake_viewer,
                panel_frames,
            ))?;
            let sink = sink.ok_or("--decoupled-render needs a sink")?;
            Ok((
                RenderThreadSink { sink, frame_hooks },
                (stop_request, vulkan),
            ))
        },
    )?;
    Ok(ViewerSinks {
        headset: None,
        decoupled: Some(decoupled),
        frame_hooks: Vec::new(),
        stop_request,
        vulkan,
    })
}

/// `QAROS_RERUN_SCENE_DELAY_MS`: milliseconds slept after every UI pass, a stand-in for a
/// scene whose update takes that long.
fn scene_delay_from_environment() -> Result<Option<std::time::Duration>, String> {
    match std::env::var("QAROS_RERUN_SCENE_DELAY_MS") {
        Ok(text) => text
            .parse()
            .map(|milliseconds| Some(std::time::Duration::from_millis(milliseconds)))
            .map_err(|error| format!("QAROS_RERUN_SCENE_DELAY_MS={text}: {error}")),
        Err(_) => Ok(None),
    }
}

fn install_view_3d_observer(
    headset: Option<&Headset>,
    capture: Option<Arc<Mutex<SceneCapture>>>,
    view_directory: SharedViewDirectory,
) {
    let headset: Option<SharedHeadset> = headset.map(|headset| Arc::clone(&headset.renderer));
    let observer = move |frame: &re_view_spatial::view_3d_observer::View3DFrame<'_, '_>| {
        {
            let mut directory = view_directory
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            directory.views_3d.insert(frame.query.view_id);
            directory.all_views = frame.viewer_ctx.query_results.keys().copied().collect();
        }
        if let Some(headset) = &headset {
            headset
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .on_view_3d_frame(frame);
        }
        if let Some(capture) = &capture {
            capture
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .on_view_3d_frame(frame);
        }
    };
    if re_view_spatial::view_3d_observer::set_view_3d_observer(Box::new(observer)).is_err() {
        re_log::error!("headset: a 3D view observer was already installed");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(ParseOutcome::Help) => {
            print!("{USAGE}");
            return Ok(());
        }
        Err(ParseOutcome::Error(message)) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let main_thread_token = rerun::MainThreadToken::i_promise_i_am_on_the_main_thread();
    re_log::setup_logging();
    re_crash_handler::install_crash_handlers(re_viewer::build_info());

    // What can fail without QAROS fails here, before the QAROS library is loaded.
    let memory_knobs = MemoryKnobs::from_environment()?;
    re_log::info!("{memory_knobs}");
    let runtime = re_viewer::AsyncRuntimeHandle::from_current_tokio_runtime_or_wasmbindgen()
        .map_err(|error| error.to_string())?;
    let grpc_address = format!("0.0.0.0:{}", options.grpc_port).parse()?;
    let view_directory = SharedViewDirectory::new(Mutex::new(ViewDirectory::default()));
    let panel_capture = start_panel_capture(&mut options, &view_directory)?;

    let loads_qaros = options.sink == SinkKind::Qaros;
    let outcome = run_viewer(
        &options,
        &memory_knobs,
        main_thread_token,
        view_directory,
        panel_capture,
        runtime,
        grpc_address,
    )
    .await;
    match exit_code_after_teardown(loads_qaros, &outcome) {
        Some(code) => {
            if let Err(error) = &outcome {
                eprintln!("Error: {error:?}");
            }
            end_process_after_teardown(code)
        }
        None => outcome,
    }
}

/// Once the QAROS library may be loaded, the process ends through
/// `end_process_after_teardown` with the code `main` would return (0, or 1 for an error): a
/// normal exit runs the detach code of QAROS DLLs, which stay mapped for the life of the process.
/// By then `run_viewer` has returned, and with it the app that held the sink, the panels and
/// the connection is gone.
fn exit_code_after_teardown<Error>(loads_qaros: bool, outcome: &Result<(), Error>) -> Option<i32> {
    loads_qaros.then_some(i32::from(outcome.is_err()))
}

/// Builds the sink, then runs the viewer until its window closes or, headless, until it is
/// done; a QAROS Stop ends the process from inside.
async fn run_viewer(
    options: &Options,
    memory_knobs: &MemoryKnobs,
    main_thread_token: rerun::MainThreadToken,
    view_directory: SharedViewDirectory,
    panel_capture: Option<PanelCapture>,
    runtime: re_viewer::AsyncRuntimeHandle,
    grpc_address: std::net::SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let graphics = GraphicsChoice::from_options(options, memory_knobs);
    let scene_delay = scene_delay_from_environment()?;
    let egui_context = Arc::new(OnceLock::new());
    let ViewerSinks {
        headset,
        decoupled,
        frame_hooks,
        stop_request,
        vulkan,
    } = build_viewer_sinks(
        options,
        memory_knobs,
        &graphics,
        &egui_context,
        panel_capture.as_ref().map(PanelCapture::frames),
    )
    .await?;
    let wgpu_setup = graphics.wgpu_setup(vulkan.as_ref());
    drop(vulkan);
    install_view_3d_observer(
        headset.as_ref(),
        decoupled
            .as_ref()
            .map(|decoupled| Arc::clone(&decoupled.capture)),
        Arc::clone(&view_directory),
    );

    let grpc_listener = re_grpc_server::ServerListener::bind(grpc_address).map_err(|error| {
        format!("cannot listen for Rerun SDK connections on {grpc_address}: {error}")
    })?;
    let (log_receiver, _grpc_server_handle) = re_grpc_server::spawn_with_recv(
        grpc_listener,
        re_grpc_server::ServerOptions::default(),
        re_grpc_server::shutdown::never(),
    );
    re_log::info!(
        "listening for Rerun SDK connections on port {}; headset sink: {:?}",
        options.grpc_port,
        options.sink
    );

    let quit_when_done = options.quit_when_done;
    let is_headless = options.headless_size.is_some();
    let build_app = move |creation_context: &eframe::CreationContext<'_>,
                          panel_capture: Option<PanelCapture>|
          -> HeadsetViewerApp {
        // Already set only if the creator ran twice, which eframe does not do.
        let _ = egui_context.set(creation_context.egui_ctx.clone());
        let mut inner = re_viewer::App::new(
            main_thread_token,
            re_viewer::build_info(),
            re_viewer::AppEnvironment::Custom("qaros-rerun".to_owned()),
            startup_options(is_headless),
            creation_context,
            None,
            runtime,
        );
        inner.add_log_receiver(log_receiver);
        HeadsetViewerApp {
            inner,
            headset,
            decoupled,
            scene_delay,
            panel_capture,
            frame_hooks,
            quit_when_done,
            stop_request,
            summary_printed: false,
            memory_budget_log: log_wgpu_device(creation_context),
        }
    };

    if let Some(size) = options.headless_size {
        #[allow(clippy::cast_precision_loss)]
        let headless_options = HeadlessOptions {
            size: egui::vec2(size[0] as f32, size[1] as f32),
            quit_when_done,
            step_interval: std::time::Duration::from_millis(4),
            wgpu_setup,
        };
        run_headless(headless_options, panel_capture, |creation_context| {
            build_app(creation_context, None)
        })?;
    } else {
        run_windowed(
            main_thread_token,
            (!graphics.is_rerun_default()).then_some(wgpu_setup),
            panel_capture,
            build_app,
        )?;
    }
    Ok(())
}

/// Headless, nobody sees the viewer's panels, so only the views are laid out: the top,
/// blueprint, selection and time panels cost UI time every viewer frame and would delay the
/// headset frames. A window keeps Rerun's defaults.
fn startup_options(is_headless: bool) -> re_viewer::StartupOptions {
    let mut startup_options = re_viewer::StartupOptions::default();
    if is_headless {
        let hidden = Some(PanelState::Hidden);
        let overrides = &mut startup_options.panel_state_overrides;
        overrides.top = hidden;
        overrides.blueprint = hidden;
        overrides.selection = hidden;
        overrides.time = hidden;
    }
    startup_options
}

/// A taken panel port (a second instance of the same demo) moves the panels to a free port, and
/// the panel pages are named by that port, so the second instance does not fail its start.
fn start_panel_capture(
    options: &mut Options,
    view_directory: &SharedViewDirectory,
) -> Result<Option<PanelCapture>, Box<dyn std::error::Error>> {
    let Some(asked_address) = options.panel_address.clone() else {
        return Ok(None);
    };
    let (server, refusal) = panel_server::PanelServer::start_or_any_free_port(&asked_address)?;
    let address = server
        .local_address()
        .map_or(asked_address.clone(), |bound| bound.to_string());
    if let Some(refusal) = refusal {
        re_log::warn!("panels: {asked_address} is taken ({refusal}); serving on {address} instead");
        options.panel_address = Some(address.clone());
    }
    re_log::info!("panels: serving 2D views under http://{address}/views");
    Ok(Some(PanelCapture::new(
        server,
        options.panel_capture_interval,
        Arc::clone(view_directory),
    )))
}

/// Logs the adapter and the first Vulkan memory budget line; the log goes on with the viewer.
fn log_wgpu_device(creation_context: &eframe::CreationContext<'_>) -> Option<MemoryBudgetLog> {
    let render_state = creation_context.wgpu_render_state.as_ref()?;
    re_log::info!(
        "wgpu adapter: {}",
        re_renderer::adapter_info_summary(&render_state.adapter.get_info())
    );
    let mut memory_budget_log = MemoryBudgetLog::for_device(render_state.device.clone());
    memory_budget_log.log_if_due();
    Some(memory_budget_log)
}

/// `wgpu_setup` replaces Rerun's own; `None` keeps it.
fn run_windowed(
    main_thread_token: rerun::MainThreadToken,
    wgpu_setup: Option<egui_wgpu::WgpuSetup>,
    mut panel_capture: Option<PanelCapture>,
    build_app: impl FnOnce(&eframe::CreationContext<'_>, Option<PanelCapture>) -> HeadsetViewerApp
    + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(wgpu_setup) = wgpu_setup else {
        re_viewer::run_native_app(
            main_thread_token,
            Box::new(move |creation_context| {
                Ok(Box::new(build_app(creation_context, panel_capture.take())))
            }),
            None,
        )?;
        return Ok(());
    };
    // `re_viewer::run_native_app` with only the wgpu setup replaced.
    let mut native_options = re_viewer::native::eframe_options(None);
    native_options.wgpu_options.wgpu_setup = wgpu_setup;
    eframe::run_native(
        "Rerun",
        native_options,
        Box::new(move |creation_context| {
            re_viewer::customize_eframe_and_setup_renderer(creation_context)?;
            Ok(Box::new(build_app(creation_context, panel_capture.take())))
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_qaros_every_end_terminates_with_the_code_main_would_return() {
        assert_eq!(
            exit_code_after_teardown(true, &Ok::<(), String>(())),
            Some(0)
        );
        assert_eq!(
            exit_code_after_teardown(true, &Err::<(), _>("window failed".to_owned())),
            Some(1)
        );
    }

    #[test]
    fn without_qaros_main_returns_as_usual() {
        assert_eq!(exit_code_after_teardown(false, &Ok::<(), String>(())), None);
        assert_eq!(
            exit_code_after_teardown(false, &Err::<(), _>("window failed".to_owned())),
            None
        );
    }
}
