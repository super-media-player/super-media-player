#!/usr/bin/env bash
# Make the media the guide's pictures show, in the folder given: an open
# movie (Big Buck Bunny, © Blender Foundation, CC BY 3.0) and made-up music.
# Needs ffmpeg, curl and python3 with Pillow. For the DVD menu also dvdauthor
# and mkisofs.
set -euo pipefail
out="$(mkdir -p "$1" && cd "$1" && pwd)"
mkdir -p "$out/Music" "$out/Videos" "$out/work"
font="$(fc-match -f '%{file}' 'Noto Sans:bold')"
cd "$out/work"

film="$out/Videos/Big Buck Bunny (2008).mp4"
if [[ ! -s "$film" ]]; then
    curl -fL -o film.mp4 "https://archive.org/download/BigBuckBunny_124/Content/big_buck_bunny_720p_surround.mp4"
    ffmpeg -v error -y -i film.mp4 -map 0:v -map 0:a -c copy -map_metadata -1 -metadata title="Big Buck Bunny" "$film"
fi

# A "series": parts of the film.
mkdir -p "$out/Videos/Meadow Tales"
for e in 1 2 3; do
    f="$out/Videos/Meadow Tales/Meadow Tales S01E0$e.mp4"
    [[ -s "$f" ]] || ffmpeg -v error -y -ss $((e * 120 - 60)) -t 110 -i "$film" -c copy "$f"
done

# Words for the film, for the subtitle pictures.
cat > "$out/Videos/Big Buck Bunny (2008).srt" <<'S'
1
00:00:28,000 --> 00:00:33,000
Good morning, meadow.

2
00:00:58,000 --> 00:01:04,000
What a lovely day for a walk.

3
00:01:15,000 --> 00:01:20,000
Who threw that apple?

4
00:01:32,000 --> 00:01:40,000
A butterfly! Come here, little one.

5
00:02:20,000 --> 00:02:28,000
You three again.
S

cat > covers.py <<'P'
import math, random, sys
from PIL import Image, ImageDraw, ImageFilter
name, seed = sys.argv[1], int(sys.argv[2])
random.seed(seed)
h = random.random()
import colorsys
c = [tuple(int(255 * v) for v in colorsys.hsv_to_rgb((h + d) % 1, s, b)) for d, s, b in ((0, .75, .35), (.08, .7, .95), (.5, .55, .95))]
im = Image.new("RGB", (600, 600))
for y in range(600):
    t = y / 599
    ImageDraw.Draw(im).line([(0, y), (600, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(c[0], c[1])))
d = ImageDraw.Draw(im, "RGBA")
kind = seed % 3
if kind == 0:
    d.ellipse([200, 140, 400, 340], fill=c[2] + (255,))
    for k in range(7):
        pts = [(x, 380 + k * 34 + 16 * math.sin(x / (60 + 9 * k) + k)) for x in range(0, 601, 6)]
        d.line(pts, fill=(255, 255, 255, 200 - 22 * k), width=5)
elif kind == 1:
    for k in range(9, 0, -1):
        r = k * 32
        d.ellipse([300 - r, 300 - r, 300 + r, 300 + r], fill=(c[2] if k % 2 else c[0]) + (230,))
else:
    for k in range(5):
        x = k * 130 - 40
        d.polygon([(x, 600), (x + 130, 200 + random.randint(0, 160)), (x + 260, 600)], fill=c[k % 3] + (190,))
    d.ellipse([420, 80, 520, 180], fill=(255, 255, 255, 230))
im.filter(ImageFilter.SMOOTH).save(name)
P

# song <artist> <album> <year> <genre> <n> <title> <base Hz> <seconds>
song() {
    local dir="$out/Music/$1/$2" f
    mkdir -p "$dir"
    [[ -s "$dir/cover.jpg" ]] || python3 covers.py "$dir/cover.jpg" "$(cksum <<< "$2" | cut -d' ' -f1)"
    f="$dir/$(printf %02d "$5") - $6.flac"
    [[ -s "$f" ]] && return
    local a=$7 beat="mod(t\,0.5)"
    ffmpeg -v error -y -f lavfi -i "aevalsrc=0.16*sin(2*PI*$a*t)*(0.6+0.4*sin(2*PI*0.2*t))+0.1*sin(2*PI*$a*1.5*t)*exp(-3*mod(t\,1))+0.08*sin(2*PI*$a*2*(1+0.25*floor(mod(t\,2)))*t)*exp(-4*mod(t\,0.25))+0.5*sin(2*PI*55*t)*exp(-14*$beat)+0.07*(random(0)-0.5)*exp(-30*mod(t+0.25\,0.5)):d=$8:s=44100" \
        -af "aecho=0.6:0.5:120:0.3,alimiter=limit=0.8" -ac 2 -metadata artist="$1" -metadata album_artist="$1" \
        -metadata album="$2" -metadata date="$3" -metadata genre="$4" -metadata track="$5" -metadata title="$6" "$f"
}
song "Nova Harbor" "Lighthouse Season" 2021 "Synthpop" 1 "First Signal" 220 64
song "Nova Harbor" "Lighthouse Season" 2021 "Synthpop" 2 "Harbor Lights" 196 70
song "Nova Harbor" "Lighthouse Season" 2021 "Synthpop" 3 "Long Way Round" 247 66
song "Nova Harbor" "Night Ferry" 2024 "Synthpop" 1 "Departure" 165 60
song "Nova Harbor" "Night Ferry" 2024 "Synthpop" 2 "Open Water" 185 62
song "Paper Lanterns" "Small Hours" 2019 "Ambient" 1 "Window Seat" 147 72
song "Paper Lanterns" "Small Hours" 2019 "Ambient" 2 "Folded Map" 175 68
song "The Quiet Engines" "Overpass" 2022 "Electronic" 1 "Green Wave" 208 64
song "The Quiet Engines" "Overpass" 2022 "Electronic" 2 "Ring Road" 233 64
song "Marlow Dean" "Porch Songs" 2017 "Folk" 1 "Kettle On" 262 61
song "Marlow Dean" "Porch Songs" 2017 "Folk" 2 "Hello, Rain" 294 63

cat > "$out/Music/Nova Harbor/Lighthouse Season/02 - Harbor Lights.lrc" <<'S'
[00:01.00]Out beyond the breakwater
[00:06.00]every light is calling home
[00:11.00]Harbor lights, carry me over
[00:16.00]Harbor lights, I'm not alone
[00:21.00]Count the waves, count the stars
[00:26.00]Turn the wheel and let it go
[00:31.00]Harbor lights, carry me over
[00:36.00]Harbor lights, I'm almost home
[00:41.00]Almost home
S

# A DVD with a menu.
if command -v dvdauthor > /dev/null && [[ ! -s "$out/Videos/Bunny DVD.iso" ]]; then
    export VIDEO_FORMAT=NTSC
    python3 - <<'P'
from PIL import Image, ImageDraw
boxes = [(60, 200, 300, 246), (60, 262, 300, 308), (60, 324, 300, 370)]
for n, col in (("h", (255, 210, 60, 255)), ("s", (255, 255, 255, 255))):
    im = Image.new("RGBA", (720, 480), (0, 0, 0, 0))
    for b in boxes:
        ImageDraw.Draw(im).rectangle(b, outline=col, width=4)
    im.save(n + ".png")
open("menu.xml", "w").write('<subpictures><stream><spu start="00:00:00.00" force="yes" highlight="h.png" select="s.png">'
    + "".join(f'<button x0="{a}" y0="{b}" x1="{c + 1}" y1="{d + 1}"/>' for a, b, c, d in boxes) + "</spu></stream></subpictures>")
P
    dt="fontfile=$font:fontcolor=white:shadowx=2:shadowy=2"
    ffmpeg -v error -y -ss 60 -t 16 -i "$film" -vf "scale=720:480,drawbox=40:120:290:270:black@0.55:fill,drawtext=$dt:text='BIG BUCK BUNNY':x=60:y=142:fontsize=28,drawtext=$dt:text='Play the film':x=80:y=212:fontsize=24,drawtext=$dt:text='Butterfly scene':x=80:y=274:fontsize=24,drawtext=$dt:text='Forest clip':x=80:y=336:fontsize=24" -target ntsc-dvd -aspect 16:9 -b:v 5000k menu0.mpg
    spumux -m dvd menu.xml < menu0.mpg > menu.mpg 2> /dev/null
    ffmpeg -v error -y -ss 30 -t 120 -i "$film" -vf scale=720:480 -target ntsc-dvd -aspect 16:9 film.mpg
    ffmpeg -v error -y -ss 300 -t 75 -i "$film" -vf scale=720:480 -target ntsc-dvd -aspect 16:9 clip.mpg
    cat > dvd.xml <<'X'
<dvdauthor dest="dvd"><vmgm><fpc>jump titleset 1 menu;</fpc></vmgm><titleset><menus><video aspect="16:9" widescreen="nopanscan"/>
<subpicture><stream id="0" mode="widescreen"/><stream id="1" mode="letterbox"/></subpicture>
<pgc entry="root"><vob file="menu.mpg"/><button>jump title 1;</button><button>jump title 1 chapter 3;</button><button>jump title 2;</button><post>jump menu 1;</post></pgc>
</menus><titles><video aspect="16:9"/><pgc><vob file="film.mpg" chapters="0,0:30,1:00,1:30"/><post>call menu;</post></pgc>
<pgc><vob file="clip.mpg"/><post>call menu;</post></pgc></titles></titleset></dvdauthor>
X
    rm -rf dvd && dvdauthor -x dvd.xml 2> /dev/null && mkisofs -quiet -dvd-video -V BIG_BUCK_BUNNY -o "$out/Videos/Bunny DVD.iso" dvd
fi
rm -rf "$out/work"
