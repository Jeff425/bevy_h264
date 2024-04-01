use std::{collections::VecDeque, sync::{mpsc::{channel, Sender}, Arc, Mutex}};

use bevy_app::{Plugin, PreUpdate, Update};
use bevy_asset::{Asset, AssetApp, AssetLoader, AssetServer, Assets, AsyncReadExt, Handle, LoadState};
use bevy_ecs::{component::Component, entity::Entity, event::{Event, EventWriter}, query::{With, Without}, schedule::IntoSystemConfigs, system::{Commands, Query, Res, ResMut}};
use bevy_reflect::TypePath;
use bevy_render::{render_asset::RenderAssetUsages, render_resource::{Extent3d, TextureDimension, TextureFormat}, texture::Image};
use bevy_time::{Real, Time};
use openh264::{decoder::{Decoder, DecoderConfig}, nal_units};
use thiserror::Error;

const BUF_SIZE: usize = 3;

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
    frame_time: f32, // 1.0 / 60.0 for 60 FPS
    
    next_frame: usize,
    frame_count: usize,

    frame_idx: usize,
    current_frame_time: f32,

    sender: Mutex<Sender<DecoderMessage>>,
    next_frame_rgb8: Arc<Mutex<VecDeque<VideoFrame>>>,
}

impl H264Decoder {
    pub fn new(images: &mut ResMut<Assets<Image>>, video: Handle<H264Video>, repeat: bool, frame_time: f32) -> Self {
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
                    let mut buffer = vec![0; width * height * 3];
                    decoded_yuv.write_rgb8(buffer.as_mut_slice());
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
            frame_time, 
            next_frame: 0,
            frame_count: 0,
            frame_idx: 0,
            current_frame_time: frame_time + 1.0,
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
    time: Res<Time<Real>>,
) {
    for (entity, mut decoder) in query.iter_mut() {
        decoder.current_frame_time += time.delta_seconds();
        if decoder.current_frame_time > decoder.frame_time {
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
                for (dest, src) in image.data.chunks_exact_mut(4).zip(frame.buffer.chunks_exact(3)) {
                    dest.copy_from_slice(&[src[2], src[1], src[0], 255]);
                }

                // Send the event
                update_ev.send(H264UpdateEvent(entity));
                decoder.current_frame_time = 0.0;
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
}

fn push_packet(
    mut query: Query<&mut H264Decoder, Without<H264DecoderLoading>>,
    videos: Res<Assets<H264Video>>,
) {
    for mut decoder in query.iter_mut() {
        // Only push more packets if there is space in the buffer
        if decoder.next_frame_rgb8.lock().unwrap().len() < BUF_SIZE {
            if let Some(video) = videos.get(&decoder.video) {
                decoder.add_video_packet(video.buffer[decoder.frame_idx].clone());
                decoder.frame_idx = (decoder.frame_idx + 1) % video.buffer.len();
            }
        }
    }
}

pub struct H264Plugin;

impl Plugin for H264Plugin {
    fn build(&self, app: &mut bevy_app::App) {
        app
            .add_event::<H264UpdateEvent>()
            .init_asset::<H264Video>()
            .init_asset_loader::<H264VideoLoader>()
            .add_systems(PreUpdate, begin_decode)
            .add_systems(Update, (push_packet, decode_video).chain());
    }
}