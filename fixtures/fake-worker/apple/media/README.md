# Apple media fixtures

Generated for the scripted worker with local ffmpeg 9.0.2. These are new fixture
assets, not copies or edits of Kotlin resources. All content is a constant
0x336699 image. `manifest.json` records bytes, SHA-256, codec, dimensions, frame
rate and duration. Replay needs neither ffmpeg nor a frame decoder.

```sh
ffmpeg -hide_banner -loglevel error -f lavfi -i color=c=0x336699:s=64x96:r=5 \
  -t 1 -an -c:v libx264 -pix_fmt yuv420p \
  -metadata creation_time=2000-01-01T00:00:00Z -y recording.mp4
ffmpeg -hide_banner -loglevel error -i recording.mp4 \
  -vf 'scale=852:852:force_original_aspect_ratio=decrease:force_divisible_by=2' \
  -preset ultrafast -movflags +faststart -y transcoded.mp4
ffmpeg -hide_banner -loglevel error -f lavfi -i color=c=0x336699:s=64x96 \
  -frames:v 1 -y screenshot.png
ffprobe -v error -select_streams v:0 \
  -show_entries stream=codec_name,width,height,pix_fmt,r_frame_rate,duration \
  -of json transcoded.mp4
```

The source video is H264/yuv420p, 64×96, 5 fps, one second. The transcode is
H264/yuv420p, 568×852, 5 fps, one second. Screenshot is PNG/RGB24, 64×96.
The actual Kotlin screenshot path decodes PNG and generates the committed
single-frame GIF, including its loop/delay metadata. Evidence retains that GIF
and proves its attachment, dimensions and content hash. Hash comparison is for
this pinned scripted media profile, not cross-version ffmpeg regeneration.
