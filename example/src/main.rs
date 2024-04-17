use bevy::{app::{App, FixedUpdate, Startup}, asset::{AssetServer, Assets, Handle}, core_pipeline::core_3d::Camera3dBundle, ecs::{event::EventReader, query::With, schedule::IntoSystemConfigs, system::{Commands, Query, Res, ResMut}}, math::Vec3, pbr::{AmbientLight, PbrBundle, StandardMaterial}, render::{mesh::{shape::Plane, Mesh}, texture::Image}, transform::components::Transform, utils::default, DefaultPlugins};
use bevy_h264::{decode_video, H264Decoder, H264DecoderLoading, H264Plugin, H264UpdateEvent};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(H264Plugin { fps: Some(120.0) })
        .add_systems(Startup, setup)
        .add_systems(FixedUpdate, modify_materials.after(decode_video))
        .run();
}

fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    let decoder = H264Decoder::new(
        &mut images,
        asset_server.load("test.h264"),
        false,
    );

    commands.spawn(PbrBundle {
        mesh: meshes.add(Plane::from_size(5.0)),
        material: materials.add(StandardMaterial {
            base_color_texture: Some(decoder.get_render_target()),
            ..default()
        }),
        ..default()
    })
    .insert(decoder)
    .insert(H264DecoderLoading {});

    commands.insert_resource(AmbientLight {
        brightness: 1000.0,
        ..default()
    });

    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(-2.0, 2.5, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });
}

fn modify_materials(
    query: Query<&Handle<StandardMaterial>, With<H264Decoder>>,
    mut update_ev: EventReader<H264UpdateEvent>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for update in update_ev.read() {
        if let Ok(asset_handle) = query.get(update.0) {
            let _ = materials.get_mut(asset_handle);
        }
    }
}