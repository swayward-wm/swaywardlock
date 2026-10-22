"""Port of rotate, text-shadow, vibrancy, reload and reload-keep.

These need compositor pixels, async decode/reload and child lifetime observations.
Positive reads settle; sleeps below preserve negative or temporal sampling windows.
"""

import re
import signal
import struct
import time
import zlib
from pathlib import Path

from lib.poll import alive, poll


BLACK = (0, 0, 0)
RED = (255, 0, 0)
GREEN = (0, 255, 0)
BLUE = (0, 0, 255)
WHITE = (255, 255, 255)
HEAD = 'background { color "#000000"; }\nanimations { off; }\n'


def _start(env, name, config, *, validate=False, debug=False, no_fade=True):
    # Names are invocation identities, not a shared lock.log readiness cache.
    log = name.replace(' ', '-') + '.log'
    assert not env.path(log).exists(), f'{name}: reused invocation log'
    if validate:
        cfg = env.path(name.replace(' ', '-') + '.kdl')
        cfg.write_text(config)
        result = env.run([env.binary, 'validate', '-c', cfg], check=False)
        env.path('validate-' + log).write_bytes(result.stdout + result.stderr)
        assert result.returncode == 0, f'{name} config does not validate: {result.stderr!r}'
    locker = env.locker(config, args=['--no-fade-in'] if no_fade else [],
                        env={'RUST_LOG': 'swaywardlock=debug'} if debug else None, log=log)
    locker.wait_locked(name)
    return locker


def _stop(locker, name='locker unlock'):
    locker.signal(signal.SIGUSR1)
    assert locker.wait_exit() == 0, f'{name}: failed to exit on SIGUSR1\n{locker.log_text()}'


def _png(path, colour, width=64, height=64):
    # Stream rows into zlib: the 144MP header-refusal fixture must not allocate 144MP here.
    r, g, b = colour
    grey = r == g == b
    row = b'\0' + (bytes([r]) * width if grey else bytes(colour) * width)
    compressor = zlib.compressobj(6)
    data = b''.join(compressor.compress(row) for _ in range(height)) + compressor.flush()

    def chunk(tag, body):
        return struct.pack('>I', len(body)) + tag + body + struct.pack('>I', zlib.crc32(tag + body))

    header = struct.pack('>IIBBBBB', width, height, 8, 0 if grey else 2, 0, 0, 0)
    Path(path).write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header)
                           + chunk(b'IDAT', data) + chunk(b'IEND', b''))


def _swap(path, colour):
    tmp = path.with_suffix('.tmp')
    _png(tmp, colour)
    tmp.replace(path)


def _test_png(env, name):
    path = env.path(name)
    env.run(['python3', env.contrib('ppm-pixel'), '--write-test-png', path])
    return path


def _points(shot, points):
    return tuple(shot.pixel(x, y) for x, y in points)


def _shadow_stats(shot):
    white = [(x, y) for y in range(shot.height) for x in range(shot.width)
             if min(shot.pixel(x, y)) > 200]
    if not white:
        return (0,) * 9
    xs, ys = zip(*white)
    x0, x1, y0, y1 = min(xs), max(xs), min(ys), max(ys)
    red = [(x, y) for y in range(max(0, y0 - 20), min(shot.height, y1 + 21))
           for x in range(max(0, x0 - 20), min(shot.width, x1 + 21))
           if (lambda p: p[0] > 120 and p[1] < 80 and p[2] < 80)(shot.pixel(x, y))]
    if not red:
        return (0, 0, 0, 0, 0, x0, y0, x1, y1)
    rx, ry = zip(*red)
    return (len(red), round(sum(rx) / len(rx), 2), round(sum(ry) / len(ry), 2),
            max(rx) - min(rx) + 1, max(ry) - min(ry) + 1, x0, y0, x1, y1)


def _shadow(env, name, until=None):
    return env.expect(name, _shadow_stats, until or (lambda s: s[7] > s[5]))


def _hwm(locker):
    match = re.search(r'^VmHWM:\s+(\d+)', Path(f'/proc/{locker.pid}/status').read_text(), re.M)
    assert match, 'VmHWM unavailable'
    return int(match[1])


def _samples(env, duration, update=None):
    start = time.monotonic()
    samples = []
    switched = False
    while (elapsed := time.monotonic() - start) <= duration:
        if update and elapsed >= .8 and not switched:
            update()
            switched = True
        samples.append((elapsed * 1000, *env.snapshot().pixel(160, 10)))
    return samples


def test_rotate(lock_env):
    env = lock_env
    cases = [
        ('90', 'shape { size 100 20; color "#ff0000"; corner-radius 0; rotate 90; }', [
            ('90, top of the bar', RED, 160, 80), ('90, bottom of the bar', RED, 160, 160),
            ('90, right of the bar is empty', BLACK, 200, 120), ('90, left of the bar is empty', BLACK, 120, 120)]),
        ('45', 'shape { size 200 10; color "#ff0000"; corner-radius 0; rotate 45; }', [
            ('45, the right end is up (counter-clockwise)', RED, 220, 60),
            ('45, the left end is down', RED, 100, 180), ('45, nothing where clockwise would be', BLACK, 220, 180)]),
        ('30', 'shape { size 200 20; color "#ff0000"; corner-radius 0; rotate 30; }', [('30, centre', RED, 160, 120)]),
    ]
    for name, config, checks in cases:
        locker = _start(env, 'rotate ' + name, HEAD + config, validate=True)
        for text, colour, x, y in checks:
            env.expect_colour('rotate: ' + text, colour, x, y)
        if name == '30':
            blended = env.expect(
                'rotate: 30, edges are antialiased',
                lambda s: sum(30 < (p := s.pixel(x, y))[0] < 225 and p[1] < 10 and p[2] < 10
                              for y in range(50, 190) for x in range(40, 280)), lambda n: n > 100)
            assert blended > 100, f'rotate: 30, edges are antialiased ({blended} blended pixels)'
        _stop(locker)
    face = _test_png(env, 'face.png')
    locker = _start(env, 'rotate image', HEAD + f'image {{ path "{face}"; size 60; corner-radius 0; rotate 90; }}', validate=True)
    env.expect_colour('rotate: image, red half on the left', RED, 145, 120)
    env.expect_colour('rotate: image, blue half on the right', BLUE, 175, 120)
    _stop(locker)
    widths = []
    for angle in ('', 'rotate 90;'):
        locker = _start(env, 'rotate label ' + str(len(widths)), HEAD + f'label {{ text "MMMMMMMM"; font "Sans Bold 20"; color "#ffffff"; {angle} }}', validate=True)
        metrics = env.expect('rotate: label readiness', lambda s: (
            s.colour_width(WHITE, (0, 0, 320, 240)), s.colour_width(WHITE, (0, 60, 320, 70)),
            s.colour_width(WHITE, (0, 170, 320, 180))), lambda m: m[0] > 0)
        widths.append(metrics[0])
        _stop(locker)
    assert widths[0] > 120 and 0 < widths[1] < 45 and metrics[1] > 0 and metrics[2] > 0, f'rotate: label turned upright (width {widths}, rows 60 and 170 lit)'
    locker = _start(env, 'rotate edge', HEAD + 'shape { size 100 20; color "#ff0000"; corner-radius 0; rotate 90; halign "left"; valign "top"; position 0 0; }', validate=True)
    for text, colour, x, y in [('the bar is in the left column', RED, 5, 50), ('right of the bar is empty', BLACK, 25, 50), ('the bar reaches the top', RED, 5, 5), ('the bar ends at y 101', BLACK, 5, 105)]:
        env.expect_colour('rotate: edge, ' + text, colour, x, y)
    _stop(locker)
    for angle, checks in [('', [('green above the box', GREEN, 200, 108), ('no green beside the box', BLACK, 148, 140)]), ('rotate 90;', [('green left of the bar', GREEN, 148, 120), ('green right of the bar', GREEN, 172, 120), ('no unrotated shadow left behind', BLACK, 200, 108)])]:
        name = '90' if angle else '0'
        config = f'shape {{ size 100 20; color "#ff0000"; corner-radius 0; {angle} shadow {{ softness 0; spread 4; offset x=0 y=0; color "#00ff00"; }}; }}'
        locker = _start(env, 'rotate shadow ' + name, HEAD + config, validate=True)
        for text, colour, x, y in checks:
            env.expect_colour(f'rotate: shadow {name}, {text}', colour, x, y)
        _stop(locker)
    values = []
    for name, angle in [('plain', ''), ('rotate 0', 'rotate 0;')]:
        locker = _start(env, 'rotate ' + name, HEAD + f'shape {{ size 100 40; color "#ff0000"; corner-radius 15; border {{ width 3; color "#00ff00"; }}; {angle} }}', validate=True)
        env.expect_colour('rotate: ' + name + ' shape drawn', RED)
        values.append(_points(env.snapshot(), [(160, 120), (108, 100), (109, 101), (112, 103)]))
        _stop(locker)
    assert values[0] == values[1], 'rotate: rotate 0 is pixel-identical to no rotate'
    env.swaymsg('output', 'HEADLESS-1', 'scale', '1.5')
    locker = _start(env, 'rotate scale 1.5', HEAD + cases[0][1], validate=True)
    for text, colour, x, y in [('top of the bar', RED, 160, 55), ("inside the bar's right side", RED, 172, 120), ('right of the bar is empty', BLACK, 180, 120)]:
        env.expect_colour('rotate: scale 1.5, ' + text, colour, x, y)
    _stop(locker)
    env.swaymsg('output', 'HEADLESS-1', 'scale', '2')
    locker = _start(env, 'rotate huge', 'background { color "#204060"; }\nanimations { off; }\nshape { size 10000 10; color "#ff0000"; rotate 30; }\nshape { size 20 20; color "#00ff00"; corner-radius 0; halign "left"; valign "top"; rotate 45; position 10 -10; }', validate=True)
    env.expect_colour('rotate: oversized shape skipped, background drawn', (32, 64, 96), 300, 220)
    env.expect_colour('rotate: oversized shape skipped, small rotated shape drawn', GREEN, 50, 50)
    assert re.search(r'shape: rotated:.*GPU texture limit', locker.log_text()), 'rotate: oversized offscreen texture is logged'
    _stop(locker)
    env.swaymsg('output', 'HEADLESS-1', 'scale', '1')
    locker = _start(env, 'rotate xray', HEAD + 'shape { size 100 20; xray; corner-radius 0; rotate 90; border { width 4; color "#00ff00"; }; }', validate=True)
    env.expect_colour('rotate: xray stays unrotated (border right of the cut-out)', GREEN, 212, 120)
    env.expect_colour('rotate: xray stays unrotated (no border above the centre)', BLACK, 160, 80)
    assert re.search(r'xray.*rotate|rotate.*xray', locker.log_text()), 'rotate: xray + rotate warns'
    _stop(locker)


def test_text_shadow(lock_env):
    env = lock_env
    block = 'text-shadow { passes 2; size 3; color "#ff0000"; }'

    def run(name, blk='', extra=''):
        locker = _start(env, 'text-shadow ' + name, HEAD + f'label {{ text "MMMM"; font "Sans Bold 40"; color "#ffffff"; {blk + ";" if blk else ""} {extra + ";" if extra else ""} }}', validate=True)
        stats = _shadow(env, 'text-shadow: ' + name + ' text readiness')
        points = _points(env.snapshot(), [(160, 120), (140, 118), (100, 120)])
        _stop(locker)
        return stats, points

    base, plain_px = run('plain')
    assert base[0] == 0 and base[5] > 0, f'text-shadow: no block, no red (white text x {base[5]}..{base[7]})'
    off, off_px = run('off', 'text-shadow { passes 0; size 3; color "#ff0000"; }')
    assert off == base, 'text-shadow: passes 0 draws no shadow and moves nothing'
    assert off_px == plain_px, 'text-shadow: passes 0 is pixel-identical to no block (3 points)'
    shadow, _ = run('on', block)
    assert shadow[0] > 50, f'text-shadow: red shadow around the text ({shadow[0]} red px, mean x {shadow[1]}, text x {shadow[5]}..{shadow[7]})'
    assert shadow[5] == base[5] and shadow[7] == base[7], 'text-shadow: the text itself did not move'
    moved, _ = run('right', 'text-shadow { passes 2; size 3; color "#ff0000"; offset x=10 y=0; }')
    dx = round(moved[1] - shadow[1], 2)
    assert 8 <= dx <= 12, f'text-shadow: offset x=10 moves the red mass right by {dx} px'
    up, _ = run('up', 'text-shadow { passes 2; size 3; color "#ff0000"; offset x=0 y=10; }')
    dy = round(shadow[2] - up[2], 2)
    assert 8 <= dy <= 12, f'text-shadow: offset y=10 moves the red mass up by {dy} px'
    big, _ = run('big', 'text-shadow { passes 3; size 6; color "#ff0000"; boost 10; }')
    assert big[3] > shadow[3], f'text-shadow: passes 3 size 6 reaches wider ({big[3]} px, was {shadow[3]})'
    zero, _ = run('zero', 'text-shadow { passes 2; size 3; color "#ff0000"; boost 0; }')
    assert zero[0] == 0, 'text-shadow: boost 0 draws no red'
    assert shadow[3] > shadow[4], f'text-shadow: unrotated red mass is wide ({shadow[3]} x {shadow[4]})'
    rot, _ = run('rot', block, 'rotate 90')
    assert rot[0] > 50 and rot[4] > rot[3], f'text-shadow: rotate 90, the red mass is tall ({rot[3]} x {rot[4]}, {rot[0]} px)'
    text = env.path('shadow-text')
    text.write_text('MM\n')
    locker = _start(env, 'text-shadow cmd label', HEAD + f'label {{ text "cmd[update:300] cat {text}"; font "Sans Bold 40"; color "#ffffff"; {block}; }}')
    before = _shadow(env, 'text-shadow: command first frame')
    text.write_text('MMMM\n')
    # Worker-24: a stable old frame is not changed-text readiness.
    after = _shadow(env, 'text-shadow: the shadow follows a changed text', lambda s: s[3] > before[3] and s[0] > before[0])
    env.swaymsg('output', 'HEADLESS-1', 'resolution', '400x300')
    _shadow(env, 'text-shadow: after a relayout the shadow is drawn', lambda s: s[0] > 50 and s[5] > after[5])
    env.swaymsg('output', 'HEADLESS-1', 'resolution', '320x240')
    _stop(locker)
    locker = _start(env, 'text-shadow soft', HEAD + 'label { text "MMMM"; font "Sans Bold 40"; color "#ffffff"; text-shadow { passes 3; size 0; color "#ff0000"; boost 1; }; }', validate=True)
    soft = _shadow(env, 'text-shadow: soft text readiness')
    shot = env.snapshot()
    reds = []
    for x in range(shot.width):
        r, g, _ = shot.pixel(x, (soft[6] + soft[8]) // 2)
        if g > 40:
            break
        reds.append(r)
    peak, first, step = max(reds), next((v for v in reds if v), 0), max(abs(a - b) for a, b in zip(reds, reds[1:]))
    assert peak > 40 and step <= 24, f'text-shadow: size 0 is soft, no hard edge (peak red {peak}, first {first}, largest step {step})'
    _stop(locker)
    text = env.path('shadow-worst')
    text.write_text('MM\n')
    env.swaymsg('output', 'HEADLESS-1', 'scale', '2')
    locker = _start(env, 'text-shadow worst config', HEAD + f'label {{ text "cmd[update:300] cat {text}"; font "Sans Bold 40"; color "#ffffff"; text-shadow {{ passes 4; size 20; color "#ff0000"; boost 10; }}; }}', debug=True)
    worst = _shadow(env, 'text-shadow: worst config draws the text')
    assert worst[7] > worst[5], 'text-shadow: worst config draws the text'
    text.write_text('MMMM\n')
    pattern = r'text shadow: (\d+)x(\d+) px'
    assert poll(lambda: len(re.findall(pattern, locker.log_text())) >= 2), 'text-shadow: blurred once per text, not again on a relayout (second text blur never completed)'
    env.swaymsg('output', 'HEADLESS-1', 'resolution', '400x300')
    time.sleep(1)  # Full negative cache-reuse observation, not readiness padding.
    sizes = re.findall(pattern, locker.log_text())
    assert len(sizes) == 2, 'text-shadow: blurred once per text, not again on a relayout'
    assert sizes and all(int(w) * int(h) <= 2**19 for w, h in sizes), f'text-shadow: worst config blurs at most 2^19 px ({sizes})'
    hwm = _hwm(locker)
    start = time.monotonic()
    _stop(locker, 'text-shadow: worst config exits on SIGUSR1')
    env.path('text-shadow-telemetry.log').write_text(
        f'text-shadow: worst config, SIGUSR1 to exit {(time.monotonic() - start)*1000:.0f} ms, peak RSS {hwm} kB\n')


def test_vibrancy(lock_env):
    env = lock_env
    for name, config, colour, tolerance, message in [
        ('boost', 'color "#e67828"; blur { passes 1; noise 0; saturation 1; }; vibrancy 1.0; vibrancy-darkness 0.05;', (255, 116, 15), 3, 'boosts the blurred colour like hyprlock'),
        ('plain', 'color "#e67828"; blur { passes 1; noise 0; saturation 1; };', (230, 120, 40), 0, 'a uniform blur without post-processing keeps the colour'),
        ('saturation', 'color "#e67828"; blur { passes 1; noise 0; saturation 0; };', (138, 138, 138), 3, 'blur saturation 0 is grey (swayward)'),
    ]:
        locker = _start(env, 'vibrancy ' + name, f'background {{ {config} }}\nanimations {{ off; }}')
        env.expect('vibrancy: ' + message, lambda s: s.pixel(), lambda p: all(abs(a-b) <= tolerance for a, b in zip(p, colour)))
        _stop(locker)
    locker = _start(env, 'vibrancy noise', 'background { color "#808080"; blur { passes 1; noise 0.2; saturation 1; }; }\nanimations { off; }')
    time.sleep(.3)  # Single-shot spatial noise measurement, source observation window.
    points = [(10, 10), (50, 20), (90, 30), (130, 40), (170, 50), (210, 60), (250, 70), (290, 80), (30, 200), (160, 120)]
    distinct = len(set(_points(env.snapshot(), points)))
    assert distinct >= 3, f'vibrancy: blur noise varies the pixels ({distinct} distinct)'
    _stop(locker)
    bg = _test_png(env, 'vibrancy-bg.png')
    locker = _start(env, 'vibrancy orientation', f'background {{ path "{bg}"; fit "fill"; blur {{ passes 1; offset 1; noise 0; saturation 0.99; }}; }}\nanimations {{ off; }}')
    time.sleep(.3)
    top, bottom = _points(env.snapshot(), [(160, 10), (160, 230)])
    assert top[0] > 200 and top[2] < 60 and bottom[2] > 200 and bottom[0] < 60, 'vibrancy: post-processed image keeps red on top'
    _stop(locker)
    locker = _start(env, 'vibrancy no-blur', 'background { color "#e67828"; vibrancy 1.0; }\nanimations { off; }')
    env.expect_colour('vibrancy: no effect without blur', (230, 120, 40))
    assert re.search(r'vibrancy.*needs blur', locker.log_text()), 'vibrancy: warns that it needs blur'
    _stop(locker)


def test_reload(lock_env):
    env = lock_env
    a = _test_png(env, 'a.png')
    b = env.path('b.png')
    _png(b, GREEN)
    for name, config, code in [
        ('three keys', 'background { reload-cmd "echo x"; reload-ms 300; crossfade-ms 500; }\nimage { path "/x.png"; reload-cmd "echo x"; reload-ms 0; }', 0),
        ('crossfade image', 'image { path "/x.png"; crossfade-ms 500; }', 1),
    ]:
        cfg = env.path('reload-validate-' + name.replace(' ', '-') + '.kdl')
        cfg.write_text(config)
        result = env.run([env.binary, 'validate', '-c', cfg], check=False)
        env.path(cfg.stem + '.log').write_bytes(result.stdout + result.stderr)
        assert result.returncode == code, 'reload: ' + ('the three keys validate' if code == 0 else 'crossfade-ms is refused on an image')
    cur = env.path('cur')

    def background(interval, extra=''):
        return f'background {{ color "#000000"; fit "fill"; reload-cmd "cat {cur}"; reload-ms {interval}; {extra} }}'

    cur.write_text(str(a) + '\n')
    locker = _start(env, 'reload cmd', 'animations { off; }\n' + background(300), no_fade=False)
    env.expect_colour('reload cmd: first picture', RED, 160, 10)
    cur.write_text(str(b) + '\n')
    env.expect_colour('reload cmd: follows the command', GREEN, 160, 10)
    _stop(locker)
    cur.write_text(str(a) + '\n')
    locker = _start(env, 'reload usr2', 'animations { off; }\n' + background(0), no_fade=False)
    env.expect_colour('reload usr2: first picture', RED, 160, 10)
    cur.write_text(str(b) + '\n')
    time.sleep(1.5)
    assert env.snapshot().pixel(160, 10) == RED, 'reload usr2: no reload without the signal'
    locker.signal(signal.SIGUSR2)
    env.expect_colour('reload usr2: SIGUSR2 reloads', GREEN, 160, 10)
    _stop(locker)
    picture = env.path('p.png')
    _png(picture, RED)
    locker = _start(env, 'reload image', HEAD + f'image {{ path "{picture}"; size 60; corner-radius 0; reload-ms 300; }}', no_fade=False)
    env.expect_colour('reload image: first picture', RED)
    _swap(picture, GREEN)
    env.expect_colour('reload image: follows the file', GREEN)
    _stop(locker)
    cur.write_text(str(a) + '\n')
    locker = _start(env, 'reload crossfade', background(300, f'path "{a}"; crossfade-ms 2000;'))
    env.expect_colour('reload crossfade: first picture', RED, 160, 10)
    time.sleep(.5)
    cur.write_text(str(b) + '\n')
    samples = _samples(env, 3.6)
    assert any(500 <= ms <= 1500 and 60 < r < 200 and 60 < g < 200 for ms, r, g, _ in samples), f'reload crossfade: a mixed colour mid-fade (no mixed sample in 0.5-1.5 s: {samples})'
    assert tuple(samples[-1][1:]) == GREEN, 'reload crossfade: ends on the new picture'
    _stop(locker)
    cur.write_text(str(a) + '\n')
    locker = _start(env, 'reload failure', 'animations { off; }\n' + background(300), no_fade=False)
    env.expect_colour('reload failure: first picture', RED, 160, 10)
    corrupt = env.path('corrupt.png')
    corrupt.write_text('not a png\n')
    cur.write_text(str(corrupt) + '\n')
    time.sleep(1.5)
    assert env.snapshot().pixel(160, 10) == RED, 'reload failure: a corrupt file keeps the picture'
    assert 'corrupt.png' in locker.log_text(), 'reload failure: warned (nothing logged)'
    assert locker.alive(), 'reload failure: the locker is alive'
    cur.write_text('')
    time.sleep(1)
    assert env.snapshot().pixel(160, 10) == RED, 'reload failure: empty output keeps the picture'
    _stop(locker, 'reload failure: unlocks')
    pidfile = env.path('cmdpid')
    locker = _start(env, 'reload unlock', f'background {{ color "#000000"; reload-cmd "echo $$ > {pidfile}; sleep 30"; reload-ms 300; }}')
    assert poll(lambda: pidfile.exists() and pidfile.stat().st_size), 'reload unlock: the command never ran'
    cmdpid = int(pidfile.read_text())
    assert alive(cmdpid), 'reload unlock: the command is running'
    start = time.monotonic()
    _stop(locker, 'reload unlock: exit code')
    ms = (time.monotonic() - start) * 1000
    assert ms < 1500, f'reload unlock: unlocked at once ({ms:.0f} ms)'
    # Assert before env.close: the fixture reaper must not make a production leak pass.
    time.sleep(.3)
    assert not alive(cmdpid), 'reload unlock: the command is gone (the command outlived the locker)'


def test_reload_keep(lock_env):
    env = lock_env
    red, green, blue = [env.path(c + '.png') for c in ('red', 'green', 'blue')]
    for path, colour in [(red, RED), (green, GREEN), (blue, BLUE)]:
        _png(path, colour)
    cur = env.path('cur')
    wide = env.path('wide.png')
    _png(wide, GREEN, 4000, 2)
    cur.write_text(str(red) + '\n')
    locker = _start(env, 'reload keep', HEAD + f'image {{ path "{red}"; size 60; corner-radius 0; reload-cmd "cat {cur}"; reload-ms 200; }}', no_fade=False)
    env.expect_colour('reload keep: first picture', RED)
    cur.write_text(str(wide) + '\n')
    time.sleep(1.5)
    for _ in range(5):
        env.run(['wtype', 'x'])
    time.sleep(.5)
    assert env.snapshot().pixel() == RED, 'reload keep: an undrawable picture keeps the old one'
    assert sum('over the GPU texture limit' in line for line in locker.log_text().splitlines()) == 1, 'reload keep: the render failure is logged once'
    _stop(locker, 'reload keep: undrawable, unlocks')
    huge = env.path('huge.png')
    _png(huge, (128, 128, 128), 12000, 12000)
    cur.write_text(str(red) + '\n')
    locker = _start(env, 'reload keep huge', f'animations {{ off; }}\nbackground {{ color "#000000"; fit "fill"; reload-cmd "cat {cur}"; reload-ms 200; }}', debug=True, no_fade=False)
    env.expect_colour('reload keep huge: first picture', RED, 160, 10)
    cur.write_text(str(huge) + '\n')
    assert locker.wait_log('huge.png', timeout=30), 'reload keep huge: no refusal logged'
    time.sleep(.5)
    assert env.snapshot().pixel(160, 10) == RED, 'reload keep huge: the picture stays'
    assert re.search(r'WARN.*huge.png.*megapixels', locker.log_text()), 'reload keep huge: refused with a warning'
    assert locker.alive(), 'reload keep huge: the locker is alive'
    hwm = _hwm(locker)
    assert hwm < 1000000, f'reload keep huge: peak memory {hwm} kB'
    large = env.path('large.png')
    _png(large, GREEN, 2000, 1000)
    cur.write_text(str(large) + '\n')
    env.expect_colour('reload keep large: the picture changes', GREEN, 160, 10)
    assert '2000x1000 px, scaled to 480x240' in locker.log_text(), 'reload keep large: scaled to the output'
    _stop(locker, 'reload keep huge: unlocks')
    cur.write_text(str(red) + '\n')
    locker = _start(env, 'reload keep fade', f'background {{ color "#000000"; fit "fill"; path "{red}"; reload-cmd "cat {cur}"; reload-ms 200; crossfade-ms 2000; }}')
    env.expect_colour('reload keep fade: first picture', RED, 160, 10)
    time.sleep(.5)
    cur.write_text(str(green) + '\n')
    samples = _samples(env, 6, lambda: cur.write_text(str(blue) + '\n'))
    assert all(r >= 30 and b <= 10 for ms, r, _, b in samples if ms <= 1700), f'reload keep fade: the running fade is not cut short (the fade jumped: {samples})'
    assert any(ms > 1700 and 60 < b < 200 for ms, _, _, b in samples), f'reload keep fade: then fades to the newest picture (no fade to blue: {samples})'
    assert tuple(samples[-1][1:]) == BLUE, 'reload keep fade: ends on the newest picture'
    _stop(locker, 'reload keep fade: unlocks')
