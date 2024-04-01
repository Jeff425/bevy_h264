# Bevy H264
This plugin is a very primitive solution to playing .h264 videos in Bevy.
Note that this can ONLY play .h264 videos and does not support b frames.
No audio support!

This is a continuation of [Bevy Video](https://github.com/PortalCloudInc/bevy_video/tree/main)

I recommend encoding your videos with ffmpeg and using the -bf 0 option and x264-param slices=1 like below
```
ffmpeg -i test.mkv -c:v libx264 -bf 0 -x264-params slices=1 test.h264
```
Your ffmpeg must be compiled with libx264

## Usage
Create the component with
```
let decoder = H264Decoder::new(
    &mut images, // ResMut<Assets<Images>>
    asset_server.load("test.h264"), // The video file to load
    false, // Repeat?
    1.0 / 60.0, // Frame Time. This will play the video at 60 FPS
);
```
Fetch the image handle with
```
decoder.get_render_target();
```
I recommend inserting the decoder component onto the entity that will use the render target handle in their material
If loading the video from a file, insert the H264DecoderLoading component
```
H264DecoderLoading {}
```

As image changes are not reflected on a material until a material has been accessed mutably, read the event
```
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
```
and add the system to your app with
```
.add_systems(Update, modify_materials.after(decode_video))
```

Pause the video by inserting the H264DecoderPause component onto your decoder entity.
If decoder.repeat == false, then at the end of the video H264DecoderPause will be inserted.

This is not hardware accelerated at all. If you want an FPS of 60+ then make sure to compile in release mode

## Patent and license on h264
Nothing below is legal advice, just my findings on the subject matter
The h264 codec has many patents related to it, listed here https://www.reddit.com/r/linux/comments/10v3op0/which_year_should_we_treat_h264_patents_as/

MPEG LA also requires a license if you are selling your application bundled with the videos
https://www.via-la.com/licensing-2/avc-h-264/avc-h-264-license-fees/
The codec itself is shipped with Windows and Mac, thus the only fees that an application should have to worry about is Section 2 Title-by-Title.
If you ship 12 minutes or less, then there is no royalty fee. Otherwise, the fee is $0.02 per title.
I am making an assumption that selling 1 copy of your application counts as 1 "title"

If you do decide to ship a commercial application with h264 videos, I highly recommend doing research and contacting a lawyer