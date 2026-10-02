"""Generate the small, original media used by the authored Atlas demo."""
from pathlib import Path
import math
import struct
import subprocess
import wave

OUTPUT = Path(__file__).resolve().parent
FONT = "/System/Library/Fonts/Supplemental/Arial.ttf"
filters = [
    "drawbox=x=460:y=0:w=180:h=360:color=0xceddd0:t=fill",
    f"drawtext=fontfile={FONT}:text='PROJECT ATLAS':x=40:y=32:fontsize=16:fontcolor=0x203b31",
    f"drawtext=fontfile={FONT}:text='A clear path ahead.':x=38:y=78:fontsize=34:fontcolor=0x203b31",
    f"drawtext=fontfile={FONT}:text='Tasks completed this week':x=40:y=139:fontsize=14:fontcolor=0x587160",
]
for index, (label, value) in enumerate([
    ("Design", 32), ("Product", 28), ("Engineering", 40), ("Operations", 24)
]):
    y = 177 + index * 34
    filters += [
        f"drawtext=fontfile={FONT}:text='{label}':x=40:y={y}:fontsize=13:fontcolor=0x203b31",
        f"drawbox=x=147:y={y}:w={value*6}:h=19:color=0x438366:t=fill:enable='gte(t,{index*.6+.4})'",
        f"drawtext=fontfile={FONT}:text='{value}':x={156+value*6}:y={y}:fontsize=13:fontcolor=0x203b31:enable='gte(t,{index*.6+.4})'",
    ]
filters += [
    f"drawtext=fontfile={FONT}:text='124':x=486:y=155:fontsize=48:fontcolor=0x203b31:enable='gte(t,2.7)'",
    f"drawtext=fontfile={FONT}:text='COMPLETE':x=487:y=212:fontsize=11:fontcolor=0x587160:enable='gte(t,2.7)'",
    f"drawtext=fontfile={FONT}:text='Weekly review / 1 October':x=40:y=323:fontsize=11:fontcolor=0x587160",
]
subprocess.run([
    "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
    "color=c=0xecf2e9:s=640x360:r=12:d=5", "-vf", ",".join(filters),
    "-an", "-c:v", "libx264", "-preset", "slow", "-crf", "28",
    "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(OUTPUT / "atlas-overview.mp4"),
], check=True)

# A quiet five-second phrase of four original synthesized notes.
rate = 22050
duration = 5
notes = [261.63, 329.63, 392.0, 523.25]
frames = bytearray()
for n in range(rate * duration):
    t = n / rate
    value = 0.0
    for i, frequency in enumerate(notes):
        local = t - i * 0.8
        if 0 <= local < 2.5:
            envelope = min(1, local / 0.025) * math.exp(-local * 2)
            value += 0.13 * envelope * (math.sin(2*math.pi*frequency*local)
                                     + 0.25 * math.sin(4*math.pi*frequency*local))
    value *= min(1, (duration - t) / 0.3)
    frames.extend(struct.pack("<h", round(max(-1, min(1, value)) * 32767)))
with wave.open(str(OUTPUT / "atlas-theme.wav"), "wb") as sound:
    sound.setnchannels(1)
    sound.setsampwidth(2)
    sound.setframerate(rate)
    sound.writeframes(frames)
