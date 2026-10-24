# Builds the app icon: a flat rounded square with three people drawn on it.
# Writes assets\icon.png (256 px, window icon) and assets\icon.ico (multi-size,
# embedded in the exe). Run: powershell -File assets\make-icon.ps1
Add-Type -AssemblyName System.Drawing

$back = [Drawing.Color]::FromArgb(255, 48, 48, 48)   # rounded square
$fg = [Drawing.Color]::FromArgb(255, 78, 78, 78)     # people
$margin = 0.03      # gap around the square, as a fraction of the icon size
$radius = 0.225     # corner radius, as a fraction of the square size
$glyphSize = 0.74   # glyph width, as a fraction of the icon size
$gap = 0.045        # outline around the front person, as a fraction of the glyph width

$dir = $PSScriptRoot

function New-RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# A head and shoulders, `$s` tall-ish, centered on x = $cx, top at $top.
# Height is 0.74 * $s, width 0.62 * $s.
function New-Person([float]$cx, [float]$top, [float]$s) {
    $p = New-Object Drawing.Drawing2D.GraphicsPath
    $r = 0.17 * $s
    $p.AddEllipse($cx - $r, $top, 2 * $r, 2 * $r)
    $p.StartFigure()
    $p.AddArc($cx - 0.31 * $s, $top + 0.40 * $s, 0.62 * $s, 0.68 * $s, 180, 180)
    $p.CloseFigure()
    return $p
}

function New-IconFrame([int]$size) {
    $bmp = New-Object Drawing.Bitmap $size, $size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([Drawing.Color]::Transparent)

    $m = [Math]::Round($size * $margin)
    $side = $size - 2 * $m
    $g.FillPath((New-Object Drawing.SolidBrush $back), (New-RoundedRect $m $m $side $side ($side * $radius)))

    # Glyph units: the front person is 1 wide and 0.74 tall (see New-Person),
    # the two behind it are 0.8 scale, so the group is 1.22 wide.
    $u = $size * $glyphSize / 1.22
    $cx = $size / 2
    $oy = ($size - 0.74 * $u) / 2
    $brush = New-Object Drawing.SolidBrush $fg
    foreach ($dx in -0.36, 0.36) {
        $g.FillPath($brush, (New-Person ($cx + $dx * $u) ($oy + 0.148 * $u) (0.8 * $u)))
    }
    $front = New-Person $cx $oy $u
    $pen = New-Object Drawing.Pen $back, ($gap * $u * 2)
    $g.DrawPath($pen, $front)
    $g.FillPath($brush, $front)
    $g.Dispose()
    return $bmp
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = @()
foreach ($sz in $sizes) {
    $frame = New-IconFrame $sz
    $ms = New-Object IO.MemoryStream
    $frame.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
    $pngs += , $ms.ToArray()
    if ($sz -eq 256) { $frame.Save((Join-Path $dir 'icon.png'), [Drawing.Imaging.ImageFormat]::Png) }
}

# --- ICO container with PNG-compressed entries (supported since Windows Vista). ---
$out = New-Object IO.MemoryStream
$w = New-Object IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $sz = $sizes[$i]; $len = $pngs[$i].Length
    $w.Write([byte]($sz % 256)); $w.Write([byte]($sz % 256))
    $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$len); $w.Write([uint32]$offset)
    $offset += $len
}
foreach ($p in $pngs) { $w.Write($p) }
[IO.File]::WriteAllBytes((Join-Path $dir 'icon.ico'), $out.ToArray())
"wrote icon.png and icon.ico ($($sizes -join ', ') px) to $dir"
