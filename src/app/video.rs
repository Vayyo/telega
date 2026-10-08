//! Videos played inside their bubble (and full window on demand): the
//! player core (`crate::av::player`) decodes, this draws its YUV frames
//! with a wgpu shader and runs the controls. The shader widget pulls the
//! current frame at draw time and asks for its own redraws (see
//! `VideoProgram`), so playback does not rebuild every window's `view` at
//! the display's pace; `VideoMsg::Tick` only does low-rate upkeep.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};
use std::time::Duration;

use iced::widget::shader::{self, Action, Viewport};
use iced::widget::{button, column, container, row, slider, space, stack, text};
use iced::{Element, Event, Fill, Rectangle, Task, mouse, window};

use super::{App, Msg, WinId};
use crate::av::player::{Frame, Player};
use crate::settings::{Account, VideoVolume};
use crate::td;

/// The video being played (one at a time).
pub(crate) struct VideoPlayback {
    pub(crate) window: WinId,
    pub(crate) chat_id: i64,
    pub(crate) message_id: i64,
    pub(crate) file_id: i32,
    pub(crate) round: bool,
    pub(crate) player: Player,
    /// Tells the download reader to give up (the player is going away).
    stop: Arc<AtomicBool>,
    /// Shown over the whole window instead of in the bubble.
    pub(crate) expanded: bool,
    /// Whether the pointer is over the compact round video.
    controls_hovered: bool,
    /// Volume before muting, to restore.
    unmuted: f32,
    /// Drawn frames tell the shader which texture set to use.
    pub(crate) id: u64,
    /// Ties this video's textures to its own lifetime: dropped with it, so
    /// `VideoPipeline::trim` frees them once this is gone, regardless of
    /// which open window's redraw happens to run `trim` next (they all
    /// share one pipeline; see `trim`'s comment).
    token: Arc<()>,
}

impl Drop for VideoPlayback {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Controls of the video player.
#[derive(Debug, Clone)]
pub(crate) enum VideoMsg {
    /// ▶ on a video bubble: play it here.
    Play(WinId, i64, i64, i32, bool),
    TogglePause,
    /// Seek bar moved (0.0..=1.0 of the duration).
    Seek(f32),
    /// Slider changes are audible immediately; disk writes wait for release.
    Volume(f32),
    VolumeReleased,
    ToggleMute,
    ToggleExpanded,
    HoverControls(bool),
    Close,
    /// Low-rate upkeep while a video is open: the picture itself redraws
    /// on its own (see `VideoProgram::update`), this only refreshes the
    /// position label and runs the fallback cleanup below.
    Tick,
}

impl App {
    pub(crate) fn on_video(&mut self, msg: VideoMsg) -> Task<Msg> {
        match msg {
            VideoMsg::Play(window, chat_id, message_id, file_id, round) => {
                if let Some(video) = &self.session.video
                    && video.window == window
                    && video.chat_id == chat_id
                    && video.message_id == message_id
                    && video.file_id == file_id
                    && video.round == round
                {
                    video.player.set_paused(!video.player.paused());
                    return Task::none();
                }
                self.persist_video_volume();
                let preference = self
                    .settings
                    .accounts
                    .iter()
                    .find(|a| a.slot == self.session.slot)
                    .and_then(|a| a.video_volumes.get(&chat_id))
                    .and_then(|chat| chat.get(&message_id))
                    .copied()
                    .unwrap_or_default();
                let size = self
                    .session
                    .files
                    .get(&file_id)
                    .map_or(0, |f| f.size.max(0) as u64);
                let stop = Arc::new(AtomicBool::new(false));
                let source =
                    td::FileStream::new(self.session.client_id, file_id, size, Arc::clone(&stop));
                let player = Player::open(source, None, true);
                player.set_volume(preference.volume);
                player.set_paused(false);
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                self.session.video = Some(VideoPlayback {
                    window,
                    chat_id,
                    message_id,
                    file_id,
                    round,
                    player,
                    stop,
                    expanded: false,
                    controls_hovered: false,
                    unmuted: preference.unmuted,
                    id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    token: Arc::new(()),
                });
                self.clear_video_thumbnail(window, chat_id, message_id);
                // Voice messages and videos do not talk over each other.
                self.playback_pause_voice();
            }
            VideoMsg::TogglePause => {
                let mut resumed = false;
                if let Some(video) = &self.session.video {
                    if video.player.ended() {
                        video.player.seek(Duration::ZERO);
                        video.player.set_paused(false);
                        resumed = true;
                    } else {
                        let was_paused = video.player.paused();
                        video.player.set_paused(!was_paused);
                        resumed = was_paused;
                    }
                }
                if resumed {
                    // Videos and voice messages do not talk over each other.
                    self.playback_pause_voice();
                }
            }
            VideoMsg::Seek(fraction) => {
                if let Some(video) = &self.session.video
                    && let Some(duration) = video.player.duration()
                {
                    video
                        .player
                        .seek(duration.mul_f32(fraction.clamp(0.0, 1.0)));
                }
            }
            VideoMsg::Volume(value) => {
                if let Some(video) = &mut self.session.video {
                    let value = value.clamp(0.0, 1.0);
                    if value > 0.0 {
                        video.unmuted = value;
                    }
                    video.player.set_volume(value);
                }
            }
            VideoMsg::VolumeReleased => self.persist_video_volume(),
            VideoMsg::ToggleMute => {
                if let Some(video) = &mut self.session.video {
                    let volume = video.player.volume();
                    if volume > 0.0 {
                        video.unmuted = volume;
                        video.player.set_volume(0.0);
                    } else {
                        video.player.set_volume(
                            if video.unmuted.is_finite() && video.unmuted > 0.0 {
                                video.unmuted.clamp(0.01, 1.0)
                            } else {
                                1.0
                            },
                        );
                    }
                    self.persist_video_volume();
                }
            }
            VideoMsg::ToggleExpanded => {
                if let Some(video) = &mut self.session.video {
                    video.expanded = !video.expanded;
                    video.controls_hovered = false;
                }
            }
            VideoMsg::HoverControls(hovered) => {
                if let Some(video) = &mut self.session.video
                    && video.round
                    && !video.expanded
                {
                    video.controls_hovered = hovered;
                }
            }
            VideoMsg::Close => {
                self.persist_video_volume();
                self.session.video = None;
            }
            VideoMsg::Tick => {
                // The chat was left through some path other than closing
                // its window or switching its chat (both already clear
                // `self.session.video` below): a fallback, and only while a video
                // is open, since this subscription is not otherwise running.
                let shown = self.session.video.as_ref().is_some_and(|v| {
                    self.session
                        .panes
                        .get(&v.window)
                        .is_some_and(|p| p.shows(v.chat_id))
                });
                if !shown {
                    self.persist_video_volume();
                    self.session.video = None;
                }
            }
        }
        Task::none()
    }

    /// The playing video for this message, not another copy of its file.
    pub(crate) fn video_of(
        &self,
        window: WinId,
        chat_id: i64,
        message_id: i64,
        file_id: i32,
        round: bool,
    ) -> Option<&VideoPlayback> {
        self.session.video.as_ref().filter(|v| {
            v.window == window
                && v.chat_id == chat_id
                && v.message_id == message_id
                && v.file_id == file_id
                && v.round == round
        })
    }

    /// Record only actual preference changes, never a drag tick.
    pub(super) fn persist_video_volume(&mut self) {
        let Some(video) = &self.session.video else {
            return;
        };
        let value = VideoVolume {
            volume: video.player.volume(),
            unmuted: video.unmuted,
        };
        let account = self
            .settings
            .accounts
            .iter_mut()
            .find(|a| a.slot == self.session.slot);
        if account
            .as_ref()
            .and_then(|a| a.video_volumes.get(&video.chat_id))
            .and_then(|chat| chat.get(&video.message_id))
            .copied()
            .unwrap_or_default()
            == value
        {
            return;
        }
        // A ready session can have no Account entry yet (before TDLib announces
        // its user); keep the preference with the session's slot nonetheless.
        let account = match account {
            Some(account) => account,
            None => {
                self.settings.accounts.push(Account {
                    slot: self.session.slot,
                    ..Account::default()
                });
                self.settings.accounts.last_mut().expect("just inserted")
            }
        };
        account
            .video_volumes
            .entry(video.chat_id)
            .or_default()
            .insert(video.message_id, value);
        self.save_settings();
    }

    /// A voice message started talking (or resumed): a playing video does
    /// not talk over it.
    pub(super) fn playback_pause_video(&mut self) {
        if let Some(video) = &self.session.video
            && !video.player.paused()
        {
            video.player.set_paused(true);
        }
    }

    /// The video does not survive its window closing: otherwise a paused
    /// or finished player, its decoder thread and the `FileStream` reading
    /// from TDLib all linger until another video is opened.
    pub(super) fn close_video_in(&mut self, window: WinId) {
        if self
            .session
            .video
            .as_ref()
            .is_some_and(|v| v.window == window)
        {
            self.persist_video_volume();
            self.session.video = None;
        }
    }

    /// Same, but only if `window` is about to show a different chat.
    pub(super) fn close_video_leaving(&mut self, window: WinId, chat_id: i64) {
        if self
            .session
            .video
            .as_ref()
            .is_some_and(|v| v.window == window && v.chat_id != chat_id)
        {
            self.persist_video_volume();
            self.session.video = None;
        }
    }

    /// The picture plus controls, `w`×`h` px (bubble) or filling the space.
    pub(crate) fn view_video<'a>(
        &'a self,
        video: &'a VideoPlayback,
        w: f32,
        h: f32,
    ) -> Element<'a, Msg> {
        let player = &video.player;
        let picture = shader::Shader::new(VideoProgram {
            id: video.id,
            round: video.round && !video.expanded,
            player,
            alive: Arc::downgrade(&video.token),
        })
        .width(if video.expanded {
            Fill
        } else {
            iced::Length::Fixed(w)
        })
        .height(if video.expanded {
            Fill
        } else {
            iced::Length::Fixed(h)
        });
        let clickable = iced::widget::mouse_area(picture)
            .on_press(Msg::Video(VideoMsg::TogglePause))
            .interaction(mouse::Interaction::Pointer);

        let controls = (!video.round || video.expanded || video.controls_hovered).then(|| {
            let position = player.position();
            let duration = player.duration();
            let fraction = duration
                .filter(|d| !d.is_zero())
                .map_or(0.0, |d| position.as_secs_f32() / d.as_secs_f32());
            let playing = !player.paused() && !player.ended();
            let button_style = |theme: &iced::Theme, status| {
                let mut style = button::text(theme, status);
                style.text_color = iced::Color::WHITE;
                style
            };
            let time = match duration {
                Some(d) => format!("{} / {}", mmss(position), mmss(d)),
                None => mmss(position),
            };
            let muted = player.volume() == 0.0;
            let seek = row![
                button(text(if playing { "⏸" } else { "▶" }).size(14))
                    .padding([2, 6])
                    .style(button_style)
                    .on_press(Msg::Video(VideoMsg::TogglePause)),
                slider(0.0..=1.0, fraction, |f| Msg::Video(VideoMsg::Seek(f)))
                    .step(0.001_f32)
                    .width(Fill),
                text(time).size(11).color(iced::Color::WHITE),
            ]
            .spacing(4)
            .align_y(iced::Center);
            let volume = row![
                button(
                    text(if muted { "🔇" } else { "🔊" })
                        .size(12)
                        .font(super::rich::EMOJI_FONT)
                )
                .padding([2, 4])
                .style(button_style)
                .on_press(Msg::Video(VideoMsg::ToggleMute)),
                slider(0.0..=1.0, player.volume(), |v| Msg::Video(
                    VideoMsg::Volume(v)
                ))
                .step(0.01_f32)
                .on_release(Msg::Video(VideoMsg::VolumeReleased))
                .width(Fill),
                button(text(if video.expanded { "⤡" } else { "⤢" }).size(14))
                    .padding([2, 4])
                    .style(button_style)
                    .on_press(Msg::Video(VideoMsg::ToggleExpanded)),
                button(text("✕").size(12))
                    .padding([2, 4])
                    .style(button_style)
                    .on_press(Msg::Video(VideoMsg::Close)),
            ]
            .spacing(4)
            .align_y(iced::Center);
            container(column![seek, volume].spacing(2))
                .width(if video.round && !video.expanded {
                    iced::Length::Fixed(w - 44.0)
                } else {
                    Fill
                })
                .padding([2, 6])
                .style(|_| container::background(iced::Color::from_rgba8(0, 0, 0, 0.7)))
        });
        let mut notice: Element<'a, Msg> = space().into();
        if let Some(e) = player.error() {
            notice = container(text(e).size(12).color(iced::Color::WHITE))
                .padding(6)
                .style(|_| container::background(iced::Color::from_rgba8(0, 0, 0, 0.7)))
                .into();
        } else if player.buffering() {
            notice = container(text("Загрузка…").size(12).color(iced::Color::WHITE))
                .padding(6)
                .style(|_| container::background(iced::Color::from_rgba8(0, 0, 0, 0.55)))
                .into();
        }
        let mut layers = stack![
            container(clickable)
                .style(move |_| {
                    let mut style = container::background(iced::Color::BLACK);
                    if video.round && !video.expanded {
                        style.border.radius = (w / 2.0).into();
                    }
                    style
                })
                .center(Fill),
            container(notice).center(Fill),
        ];
        if let Some(controls) = controls {
            layers = layers.push(column![
                space().height(Fill),
                container(controls).center_x(Fill),
                space().height(if video.round && !video.expanded {
                    44.0
                } else {
                    0.0
                }),
            ]);
        }
        layers
            .width(if video.expanded {
                Fill
            } else {
                iced::Length::Fixed(w)
            })
            .height(if video.expanded {
                Fill
            } else {
                iced::Length::Fixed(h)
            })
            .into()
    }

    /// The expanded player over the whole window.
    pub(crate) fn view_video_overlay(&self, window: WinId) -> Option<Element<'_, Msg>> {
        let video = self
            .session
            .video
            .as_ref()
            .filter(|v| v.window == window && v.expanded)?;
        let close = button(text("×").size(16))
            .style(button::secondary)
            .on_press(Msg::Video(VideoMsg::ToggleExpanded));
        let open = button(text("Открыть в системе").size(13))
            .style(button::secondary)
            .on_press(Msg::FileOpen(video.file_id));
        Some(
            iced::widget::mouse_area(
                container(
                    column![
                        row![space().width(Fill), open, close].spacing(8),
                        self.view_video(video, 0.0, 0.0),
                    ]
                    .spacing(8),
                )
                .padding(12)
                .width(Fill)
                .height(Fill)
                .style(|_| container::background(iced::Color::from_rgba8(0, 0, 0, 0.92))),
            )
            .on_press(Msg::Ignore)
            .on_scroll(|_| Msg::Ignore)
            .into(),
        )
    }
}

fn mmss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

// ---- Drawing: YUV planes to the screen ------------------------------------

struct VideoProgram<'a> {
    id: u64,
    round: bool,
    player: &'a Player,
    /// See `Planes::alive`.
    alive: Weak<()>,
}

#[derive(Default)]
struct VideoHoverState {
    id: Option<u64>,
    hovered: bool,
}

impl shader::Program<Msg> for VideoProgram<'_> {
    type State = VideoHoverState;
    type Primitive = VideoPrimitive;

    /// Frames do not arrive as application messages any more: on every
    /// redraw of this window the runtime replays a `RedrawRequested` event
    /// through here first, and while the video can still change, asking
    /// for one more redraw keeps it going. This never touches `view`, so
    /// with several windows open only the one actually playing repaints
    /// (see `app.rs`'s subscription and the module doc for why that used
    /// to not be true).
    fn update(
        &self,
        state: &mut VideoHoverState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Msg>> {
        if state.id != Some(self.id) {
            state.id = Some(self.id);
            state.hovered = false;
        }

        if self.round {
            // A stacked control can make the shader's cursor levitate; its
            // physical position still counts as hovering over the picture.
            let position = match cursor {
                mouse::Cursor::Available(position) | mouse::Cursor::Levitating(position) => {
                    Some(position)
                }
                mouse::Cursor::Unavailable => None,
            };
            let hovered = position.is_some_and(|position| {
                let radius = bounds.width.min(bounds.height) / 2.0;
                let dx = position.x - (bounds.x + bounds.width / 2.0);
                let dy = position.y - (bounds.y + bounds.height / 2.0);
                dx * dx + dy * dy <= radius * radius
            });
            if hovered != state.hovered {
                state.hovered = hovered;
                // Publishing also schedules a redraw, including when this
                // transition coincides with an active player's redraw.
                return Some(Action::publish(Msg::Video(VideoMsg::HoverControls(
                    hovered,
                ))));
            }
        } else {
            state.hovered = false;
        }

        let redrawing = matches!(event, Event::Window(window::Event::RedrawRequested(_)));
        (redrawing && player_active(self.player)).then(Action::request_redraw)
    }

    fn draw(
        &self,
        _state: &VideoHoverState,
        _cursor: mouse::Cursor,
        _bounds: Rectangle,
    ) -> VideoPrimitive {
        VideoPrimitive {
            id: self.id,
            round: self.round,
            frame: self.player.frame(),
            alive: self.alive.clone(),
        }
    }
}

/// Whether the picture can still change: a paused or ended video does not
/// need more redraws (its frame is already the one on screen), but a
/// buffering one might resume the moment more data arrives.
fn player_active(player: &Player) -> bool {
    (!player.paused() && !player.ended()) || player.buffering()
}

#[derive(Debug)]
pub(crate) struct VideoPrimitive {
    id: u64,
    round: bool,
    frame: Option<Arc<Frame>>,
    alive: Weak<()>,
}

/// Textures of one video.
struct Planes {
    size: (u32, u32),
    textures: [wgpu::Texture; 3],
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    /// Serial of the frame now in the textures.
    uploaded: u64,
    /// The `VideoPlayback::token` of the video that owns this texture set;
    /// freed once it upgrades to nothing, i.e. once that video is gone.
    /// Not a per-window "drawn last frame" flag: iced_wgpu's primitive
    /// storage is one `Arc<RwLock<..>>` shared by every open window, and
    /// `trim` runs after EACH window's own frame, so a bystander window
    /// that never draws this video would otherwise evict the textures the
    /// window actually playing it just uploaded.
    alive: Weak<()>,
}

pub(crate) struct VideoPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// sRGB target: the shader's gamma-encoded colors are linearised.
    srgb: bool,
    videos: HashMap<u64, Planes>,
}

const SHADER: &str = r#"
struct Uniforms { scale: vec2<f32>, srgb: f32, round: f32 };
@group(0) @binding(0) var ty: texture_2d<f32>;
@group(0) @binding(1) var tu: texture_2d<f32>;
@group(0) @binding(2) var tv: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> u: Uniforms;

struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    var corners = array<vec2<f32>, 4>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0), vec2(1.0, 1.0));
    let c = corners[i];
    var out: Out;
    out.pos = vec4(c * u.scale, 0.0, 1.0);
    out.uv = vec2((c.x + 1.0) * 0.5, (1.0 - c.y) * 0.5);
    return out;
}

@fragment fn fs(in: Out) -> @location(0) vec4<f32> {
    if (u.round > 0.5) {
        let center = in.uv * 2.0 - vec2(1.0);
        if (dot(center, center) > 1.0) {
            discard;
        }
    }
    // BT.709, limited range (what phone videos use).
    let y = (textureSample(ty, samp, in.uv).r - 0.0627) * 1.1644;
    let cb = textureSample(tu, samp, in.uv).r - 0.5;
    let cr = textureSample(tv, samp, in.uv).r - 0.5;
    var rgb = clamp(vec3(
        y + 1.7927 * cr,
        y - 0.2132 * cb - 0.5329 * cr,
        y + 2.1124 * cb), vec3(0.0), vec3(1.0));
    if (u.srgb > 0.5) {
        rgb = pow(rgb, vec3(2.2));
    }
    return vec4(rgb, 1.0);
}
"#;

impl shader::Pipeline for VideoPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video"),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            srgb: format.is_srgb(),
            videos: HashMap::new(),
        }
    }

    /// Textures of a video whose `VideoPlayback` is gone are freed; see
    /// `Planes::alive` for why this is not a per-window "drawn last frame"
    /// flag.
    fn trim(&mut self) {
        self.videos
            .retain(|_, planes| planes.alive.strong_count() > 0);
    }
}

impl shader::Primitive for VideoPrimitive {
    type Pipeline = VideoPipeline;

    fn prepare(
        &self,
        pipeline: &mut VideoPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let Some(frame) = &self.frame else {
            return;
        };
        let size = (frame.width, frame.height);
        let recreate = pipeline.videos.get(&self.id).is_none_or(|p| p.size != size);
        if recreate {
            let planes = create_planes(pipeline, device, size, self.alive.clone());
            pipeline.videos.insert(self.id, planes);
        }
        let planes = pipeline.videos.get_mut(&self.id).expect("just inserted");
        if planes.uploaded != frame.serial {
            planes.uploaded = frame.serial;
            let (w, h) = size;
            for (texture, (data, pw, ph)) in planes.textures.iter().zip([
                (&frame.y, w, h),
                (&frame.u, w / 2, h / 2),
                (&frame.v, w / 2, h / 2),
            ]) {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(pw),
                        rows_per_image: Some(ph),
                    },
                    wgpu::Extent3d {
                        width: pw,
                        height: ph,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        // Letterbox: the picture keeps its shape inside the widget.
        let (frame_ratio, box_ratio) = (
            size.0 as f32 / size.1.max(1) as f32,
            bounds.width / bounds.height.max(1.0),
        );
        let scale = if frame_ratio > box_ratio {
            [1.0, box_ratio / frame_ratio]
        } else {
            [frame_ratio / box_ratio, 1.0]
        };
        let uniforms = [
            scale[0],
            scale[1],
            if pipeline.srgb { 1.0 } else { 0.0 },
            if self.round { 1.0 } else { 0.0 },
        ];
        let mut bytes = [0u8; 16];
        for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(uniforms) {
            chunk.copy_from_slice(&value.to_ne_bytes());
        }
        queue.write_buffer(&planes.uniforms, 0, &bytes);
    }

    fn draw(&self, pipeline: &VideoPipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(planes) = pipeline.videos.get(&self.id) else {
            return true;
        };
        if self.frame.is_none() {
            return true;
        }
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &planes.bind_group, &[]);
        pass.draw(0..4, 0..1);
        true
    }
}

fn create_planes(
    pipeline: &VideoPipeline,
    device: &wgpu::Device,
    (w, h): (u32, u32),
    alive: Weak<()>,
) -> Planes {
    let texture = |w, h| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video plane"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    };
    let textures = [texture(w, h), texture(w / 2, h / 2), texture(w / 2, h / 2)];
    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("video uniforms"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let views: Vec<wgpu::TextureView> = textures
        .iter()
        .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
        .collect();
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("video"),
        layout: &pipeline.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&views[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&views[1]),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&views[2]),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: uniforms.as_entire_binding(),
            },
        ],
    });
    Planes {
        size: (w, h),
        textures,
        bind_group,
        uniforms,
        uploaded: 0,
        alive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::shader::{Pipeline as _, Primitive as _, Program as _};

    #[cfg(target_os = "linux")]
    #[test]
    fn close_to_tray_closing_main_stops_only_its_video() {
        use std::sync::atomic::Ordering;

        // Check the other window first so this side of the ownership contract
        // runs even while the main-window cleanup is still missing.
        for main_owns_video in [false, true] {
            let mut app = crate::app::tests::app();
            let main = app.main_window;
            let second = WinId::unique();
            app.session
                .panes
                .insert(second, crate::app::pane::ChatPane::default());
            let owner = if main_owns_video { main } else { second };
            let stop = Arc::new(AtomicBool::new(false));
            let token = Arc::new(());
            app.session.video = Some(VideoPlayback {
                window: owner,
                chat_id: 1,
                message_id: 1,
                file_id: 1,
                round: false,
                player: Player::open(
                    std::io::Cursor::new(vec![0x1a, 0x45, 0xdf, 0xa3, 1, 2, 3]),
                    None,
                    false,
                ),
                stop: Arc::clone(&stop),
                expanded: false,
                controls_hovered: false,
                unmuted: 1.0,
                id: 1,
                token: Arc::clone(&token),
            });

            let _ = app.update(Msg::WindowClosed(main));

            assert!(
                app.session.panes.contains_key(&main),
                "main pane stays for tray restore"
            );
            if main_owns_video {
                assert!(
                    app.session.video.is_none(),
                    "closed main must release its video"
                );
                assert!(
                    stop.load(Ordering::Relaxed),
                    "the released video must stop its reader"
                );
            } else {
                let video = app
                    .session
                    .video
                    .as_ref()
                    .expect("other window's video survives");
                assert_eq!(video.window, second);
                assert!(Arc::ptr_eq(&video.token, &token), "same video must survive");
                assert!(
                    !stop.load(Ordering::Relaxed),
                    "other window's reader must not stop"
                );
            }
        }
    }

    /// Draws one solid-color frame into an offscreen texture on the real
    /// GPU and reads it back: the pipeline, the upload and the YUV → RGB
    /// conversion all run. Skipped (passes) where no adapter exists.
    #[test]
    fn yuv_frames_come_out_in_their_colors() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let Ok(adapter) = iced::futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        ) else {
            eprintln!("no GPU adapter; skipped");
            return;
        };
        let (device, queue) = iced::futures::executor::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .expect("device");
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut pipeline = VideoPipeline::new(&device, &queue, format);

        // (Y, Cb, Cr) of BT.709 limited range and the RGB they mean.
        for ((y, cb, cr), rgb) in [
            ((63u8, 102u8, 240u8), [255u8, 0, 0]),
            ((173, 42, 26), [0, 255, 0]),
            ((32, 240, 118), [0, 0, 255]),
            ((235, 128, 128), [255, 255, 255]),
        ] {
            // Rectangular (including expanded) video keeps its corners;
            // round bubble video masks only the corners, not its center.
            for round in [false, true] {
                let (w, h) = (8u32, 8u32);
                let frame = Arc::new(Frame {
                    width: w,
                    height: h,
                    y: vec![y; (w * h) as usize],
                    u: vec![cb; (w * h / 4) as usize],
                    v: vec![cr; (w * h / 4) as usize],
                    pts: Duration::ZERO,
                    serial: crate::av::player::next_serial(),
                });
                let primitive = VideoPrimitive {
                    id: 1,
                    round,
                    frame: Some(frame),
                    alive: Weak::new(),
                };
                let bounds =
                    Rectangle::new(iced::Point::ORIGIN, iced::Size::new(w as f32, h as f32));
                let viewport = Viewport::with_physical_size(iced::Size::new(w, h), 1.0);
                primitive.prepare(&mut pipeline, &device, &queue, &bounds, &viewport);

                let target = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = target.create_view(&Default::default());
                // Rows of a copy are padded to 256 bytes.
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256 * u64::from(h),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    assert!(primitive.draw(&pipeline, &mut pass));
                }
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &target,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(h),
                        },
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);
                readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .expect("poll");
                let data = readback.slice(..).get_mapped_range();
                let center = &data[(4 * 256 + 4 * 4) as usize..(4 * 256 + 4 * 4 + 3) as usize];
                for (got, want) in center.iter().zip(rgb) {
                    assert!(got.abs_diff(want) <= 6, "{center:?} instead of {rgb:?}");
                }
                let corner = &data[..3];
                let expected = if round { [0, 0, 0] } else { rgb };
                for (got, want) in corner.iter().zip(expected) {
                    assert!(
                        got.abs_diff(want) <= 6,
                        "round={round}: corner {corner:?} instead of {expected:?}"
                    );
                }
                drop(data);
                readback.unmap();
            }
        }
    }

    /// Once a video errors out (or plainly ends), its shader widget must
    /// stop asking the runtime for more redraws: otherwise a paused window
    /// would spin at full rate forever over a frame that never changes.
    #[test]
    fn program_stops_asking_for_redraws_once_the_video_cannot_change() {
        let player = Player::open(
            std::io::Cursor::new(vec![0x1a, 0x45, 0xdf, 0xa3, 1, 2, 3]),
            None,
            false,
        );
        let start = std::time::Instant::now();
        while player.error().is_none() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !player_active(&player),
            "a failed video has nothing left to draw"
        );

        let program = VideoProgram {
            id: 1,
            round: false,
            player: &player,
            alive: Weak::new(),
        };
        let redraw = Event::Window(window::Event::RedrawRequested(iced::time::Instant::now()));
        let action: Option<Action<Msg>> = program.update(
            &mut VideoHoverState::default(),
            &redraw,
            Rectangle::default(),
            mouse::Cursor::default(),
        );
        assert!(
            action.is_none(),
            "must not keep requesting redraws once nothing moves"
        );
    }
}
