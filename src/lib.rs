use std::{collections::VecDeque, sync::{mpsc::{channel, Sender}, Arc, Mutex}};

use bevy_app::{FixedUpdate, Plugin, PreUpdate, Update};
use bevy_asset::{Asset, AssetApp, AssetLoader, AssetServer, Assets, AsyncReadExt, Handle, LoadState};
use bevy_ecs::{component::Component, entity::Entity, event::{Event, EventReader, EventWriter}, query::{Has, With, Without}, schedule::IntoSystemConfigs, system::{Commands, Query, Res, ResMut}};
use bevy_reflect::TypePath;
use bevy_render::{render_asset::RenderAssetUsages, render_resource::{Extent3d, TextureDimension, TextureFormat}, texture::Image};
use bevy_time::{Fixed, Time};
use openh264::{decoder::{DecodedYUV, Decoder, DecoderConfig}, nal_units};
use thiserror::Error;

const BUF_SIZE: usize = 10;

#[derive(Asset, TypePath)]
pub struct H264Video {
    buffer: Vec<Vec<u8>>,
}

#[derive(Default)]
pub struct H264VideoLoader;

#[derive(Debug, Error)]
pub enum H264VideoLoaderError {
    #[error("Could not load video: {0}")]
    Io(#[from] std::io::Error),
}

impl AssetLoader for H264VideoLoader{
    type Asset = H264Video;

    type Settings = ();

    type Error = H264VideoLoaderError;

    fn load<'a>(
        &'a self,
        reader: &'a mut bevy_asset::io::Reader,
        _settings: &'a Self::Settings,
        _load_context: &'a mut bevy_asset::LoadContext,
    ) -> bevy_asset::BoxedFuture<'a, Result<Self::Asset, Self::Error>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await?;
            let buffer = nal_units(bytes.as_slice()).map(|nal| nal.to_vec()).collect();
            Ok(H264Video {
                buffer,
            })
        })
    }

    fn extensions(&self) -> &[&str] {
        &["h264"]
    }
}

enum DecoderMessage {
    Frame(Vec<u8>),
    Stop,
}

struct VideoFrame {
    buffer: Vec<u8>,
    width: usize,
    height: usize,
}

#[derive(Component)]
pub struct H264Decoder {
    video: Handle<H264Video>,
    render_target: Handle<Image>,
    repeat: bool,
    
    next_frame: usize,
    frame_count: usize,

    frame_idx: usize,

    sender: Mutex<Sender<DecoderMessage>>,
    next_frame_rgb8: Arc<Mutex<VecDeque<VideoFrame>>>,
}

impl H264Decoder {
    pub fn new(images: &mut ResMut<Assets<Image>>, video: Handle<H264Video>, repeat: bool) -> Self {
        let render_target = images.add(Image::new_fill(
            Extent3d {
                width: 12,
                height: 12,
                depth_or_array_layers: 1,
            }, 
            TextureDimension::D2,
            &[0, 0, 0, 0],
            TextureFormat::Bgra8UnormSrgb, 
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        ));
        let (sender, receiver) = channel::<DecoderMessage>();
        let next_frame_rgb8 = Arc::new(Mutex::new(VecDeque::<VideoFrame>::with_capacity(BUF_SIZE + 1)));
        std::thread::spawn({
            let next_frame_rgb8 = next_frame_rgb8.clone();
            move || {
                let cfg = DecoderConfig::new();
                let mut decoder = Decoder::with_config(cfg).expect("Failed to create decoder");
                while let Ok(video_packet) = receiver.recv() {
                    let video_packet = match video_packet {
                        DecoderMessage::Frame(vp) => vp,
                        DecoderMessage::Stop => return,
                    };
                    let decoded_yuv = decoder.decode(video_packet.as_slice());
                    let decoded_yuv = match decoded_yuv {
                        Ok(decoded) => decoded,
                        Err(_) => {continue},
                    };
                    let Some(decoded_yuv) = decoded_yuv else {continue};

                    let (width, height) = decoded_yuv.dimension_rgb();
                    let buffer = decoded_yuv.write_bgra8();
                    let frame = VideoFrame {
                        buffer,
                        width,
                        height,
                    };
                    if let Ok(mut queue) = next_frame_rgb8.lock() {
                        queue.push_back(frame);
                    }
                }
            }
        });
        Self {
            video,
            render_target: render_target.clone(),
            repeat,
            next_frame: 0,
            frame_count: 0,
            frame_idx: 0,
            sender: Mutex::new(sender),
            next_frame_rgb8,
        }
    }

    pub fn get_render_target(&self) -> Handle<Image> {
        self.render_target.clone()
    }

    fn add_video_packet(&self, video_packet: Vec<u8>) {
        self.sender.lock().expect("Could not get lock on sender").send(DecoderMessage::Frame(video_packet)).expect("Could not send packet to decoder");
    }

    fn take_frame(&mut self) -> Option<VideoFrame> {
        if let Ok(mut queue) = self.next_frame_rgb8.lock() {
            queue.pop_front()
        } else {
            None
        }
    }
}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        self.sender.lock().expect("Could not get lock on sender").send(DecoderMessage::Stop).expect("Could not send end packet to decoder");
    }
}

// Add this component to an entity that is loading a video from the asset server
#[derive(Component)]
pub struct H264DecoderLoading;

// This update is called whenever a decoder has updated the render target image
// Make sure all materials that read the image are modified
#[derive(Event)]
pub struct H264UpdateEvent(pub Entity);

#[derive(Component)]
pub struct H264DecoderPause;

// Remove the loading flag once a video is done loading
fn begin_decode(
    mut commands: Commands,
    mut query: Query<(Entity, &mut H264Decoder), With<H264DecoderLoading>>,
    asset_server: Res<AssetServer>,
    videos: Res<Assets<H264Video>>,
) {
    for (entity, mut decoder) in query.iter_mut() {
        // If it is still loading, then ignore
        if match asset_server.get_load_state(&decoder.video) {
            Some(load_state) => matches!(load_state, LoadState::Loading),
            _ => false,
        } {
            continue;
        }
        commands.entity(entity).remove::<H264DecoderLoading>();
        
        if match asset_server.get_load_state(&decoder.video) {
            Some(load_state) => matches!(load_state, LoadState::Failed) || matches!(load_state, LoadState::NotLoaded),
            _ => false,
        } {
            commands.entity(entity).remove::<H264Decoder>();
        } else {
            if let Some(video) = videos.get(&decoder.video) {
                // Assume 1 slice per frame
                decoder.frame_count = video.buffer.len();
            }
        }
    }
}

pub fn decode_video(
    mut commands: Commands,
    mut query: Query<(Entity, &mut H264Decoder), (Without<H264DecoderPause>, Without<H264DecoderLoading>)>,
    mut images: ResMut<Assets<Image>>,
    mut update_ev: EventWriter<H264UpdateEvent>,
) {
    for (entity, mut decoder) in query.iter_mut() {
        if let Some(frame) = decoder.take_frame() {
            let image = match images.get_mut(&decoder.render_target) {
                Some(image) => image,
                None => {
                    // Render target is missing, remove self
                    println!("Render target is missing");
                    commands.entity(entity).remove::<H264Decoder>();
                    continue;
                }
            };
            if image.texture_descriptor.size.width != frame.width as u32 || image.texture_descriptor.size.height != frame.height as u32 {
                image.resize(Extent3d { width: frame.width as u32, height: frame.height as u32, depth_or_array_layers: 1 });
            }

            image.data = frame.buffer;

            // Send the event
            update_ev.send(H264UpdateEvent(entity));
            decoder.next_frame = decoder.next_frame + 1;
            if decoder.next_frame >= decoder.frame_count {
                decoder.next_frame = 0;
                if !decoder.repeat {
                    commands.entity(entity).insert(H264DecoderPause {});
                }
            }                
        }
        // If frame is missed, wait until next game tick
    }
}

fn push_packet(
    mut query: Query<&mut H264Decoder, (Without<H264DecoderLoading>, Without<H264DecoderPause>)>,
    videos: Res<Assets<H264Video>>,
) {
    for mut decoder in query.iter_mut() {
        // Only push more packets if there is space in the buffer
        let mut buffer_size = decoder.next_frame_rgb8.lock().unwrap().len();
        if let Some(video) = videos.get(&decoder.video) {
            while buffer_size < BUF_SIZE {
                decoder.add_video_packet(video.buffer[decoder.frame_idx].clone());
                decoder.frame_idx = (decoder.frame_idx + 1) % video.buffer.len();
                buffer_size += 1;
            }
        }
    }
}

// This event makes no garuntees on what the real frame will be
// If the video is not suppose to restart, then you should make sure it has been paused (ideally for a short amount of time)
// If the video is paused it will clear out the image queue
#[derive(Event)]
pub struct H264RestartEvent(Entity);

fn restart_video(
    mut query: Query<(&mut H264Decoder, Has<H264DecoderPause>), Without<H264DecoderLoading>>,
    mut restart_ev: EventReader<H264RestartEvent>,
) {
    for event in restart_ev.read() {
        if let Ok((mut decoder, is_paused)) = query.get_mut(event.0) {
            decoder.frame_idx = 0;
            decoder.next_frame = 0;
            if is_paused {
                decoder.next_frame_rgb8.lock().unwrap().clear();
            }
        }
    }
}

// Skips a step of copying by just creating the buffer in the right format
trait Bgra8Writer {
    fn write_bgra8(&self) -> Vec<u8>;
}
impl<'a> Bgra8Writer for DecodedYUV<'a> {
    fn write_bgra8(&self) -> Vec<u8> {
        let dim = self.dimension_rgb();
        let strides = self.strides_yuv();
        let size = dim.0 * dim.1 * 4;

        let mut result = vec![0; size];

        for y in 0..dim.1 {
            for x in 0..dim.0 {
                let base_tgt = (y * dim.0 + x) * 4;
                let base_y = y * strides.0 + x;
                let base_u = (y / 2 * strides.1) + (x / 2);
                let base_v = (y / 2 * strides.2) + (x / 2);

                let bgra_pixel = &mut result[base_tgt..base_tgt + 4];

                let y = self.y_with_stride()[base_y] as f32;
                let u = self.u_with_stride()[base_u] as f32;
                let v = self.v_with_stride()[base_v] as f32;

                bgra_pixel[2] = (y + 1.402 * (v - 128.0)) as u8;
                bgra_pixel[1] = (y - 0.344 * (u - 128.0) - 0.714 * (v - 128.0)) as u8;
                bgra_pixel[0] = (y + 1.772 * (u - 128.0)) as u8;
                bgra_pixel[3] = 255;
            }
        }
        result
    }
}

// Sets the fixed timestep to the given FPS
// If fixed timestep is already set, then set this to None
// All videos will play at the same FPS
pub struct H264Plugin {
    pub fps: Option<f64>,
}

impl Plugin for H264Plugin {
    fn build(&self, app: &mut bevy_app::App) {
        if let Some(fps) = self.fps {
            app.insert_resource(Time::<Fixed>::from_hz(fps));
        }
        app
            .add_event::<H264UpdateEvent>()
            .add_event::<H264RestartEvent>()
            .init_asset::<H264Video>()
            .init_asset_loader::<H264VideoLoader>()
            .add_systems(PreUpdate, begin_decode)
            .add_systems(FixedUpdate, decode_video)
            .add_systems(Update, (push_packet, restart_video).chain());
    }
}