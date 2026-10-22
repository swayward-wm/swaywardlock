"""Screenshots (one grim run each, parsed in process) and functions over them."""

import subprocess
from dataclasses import dataclass

# Colour channels within this distance count as the same colour in region measurements, which
# compare antialiased edges and gradients (exact pixel checks use `Screenshot.pixel`).
TOLERANCE = 60


@dataclass(frozen=True)
class Screenshot:
    width: int
    height: int
    data: bytes  # RGB, 3 bytes per pixel, row by row

    def pixel(self, x=None, y=None):
        """The (r, g, b) at (x, y); default the centre."""
        x = self.width // 2 if x is None else x
        y = self.height // 2 if y is None else y
        if not (0 <= x < self.width and 0 <= y < self.height):
            raise ValueError(f"pixel ({x}, {y}) is outside the {self.width}x{self.height} image")
        o = (y * self.width + x) * 3
        return tuple(self.data[o : o + 3])

    def matching(self, want, box, tolerance=TOLERANCE):
        """The (x, y) points in box (x0, y0, x1, y1), end exclusive, whose colour is near `want`."""
        x0, y0, x1, y1 = box
        return [
            (x, y)
            for y in range(y0, y1)
            for x in range(x0, x1)
            if all(abs(a - b) < tolerance for a, b in zip(self.pixel(x, y), want))
        ]

    def colour_width(self, want, box, tolerance=TOLERANCE):
        """Width in pixels of the region near colour `want` within box; 0 if there is none."""
        xs = [x for x, _ in self.matching(want, box, tolerance)]
        return max(xs) - min(xs) + 1 if xs else 0


def parse_ppm(data):
    """Parse a binary PPM (P6, maxval 255), as grim -t ppm writes it."""
    # Header: "P6", width, height, maxval, each separated by whitespace, then one whitespace byte.
    parts, i = [], 0
    while len(parts) < 4:
        while i < len(data) and data[i : i + 1].isspace():
            i += 1
        j = i
        while j < len(data) and not data[j : j + 1].isspace():
            j += 1
        if j == i:
            raise ValueError("empty or truncated PPM header")
        parts.append(data[i:j])
        i = j
    i += 1
    if parts[0] != b"P6" or not (parts[1].isdigit() and parts[2].isdigit()) or parts[3] != b"255":
        raise ValueError(f"not an 8-bit binary PPM: header {parts!r}")
    w, h = int(parts[1]), int(parts[2])
    body = data[i : i + w * h * 3]
    if len(body) != w * h * 3:
        raise ValueError(f"truncated PPM: {len(body)} of {w * h * 3} pixel bytes")
    return Screenshot(w, h, body)


def grab(env):
    """One screenshot of the whole layout through grim."""
    out = subprocess.run(
        ["grim", "-t", "ppm", "-"], env=env, capture_output=True, timeout=10, check=True
    ).stdout
    return parse_ppm(out)
