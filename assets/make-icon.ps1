# Builds the app icon in the style of the Helium browser icon: a flat rounded
# square with one centered glyph. The glyph is the people from the original
# Roblox Account Manager icon (assets\source\team_KX4_icon.ico).
# Writes assets\icon.png (256 px, window icon) and assets\icon.ico (multi-size,
# embedded in the exe). Run: powershell -File assets\make-icon.ps1
Add-Type -AssemblyName System.Drawing

$back = [Drawing.Color]::FromArgb(255, 48, 48, 48)   # rounded square
$fg = [Drawing.Color]::FromArgb(255, 78, 78, 78)     # people
$margin = 0.03      # gap around the square, as a fraction of the icon size
$radius = 0.225     # corner radius, as a fraction of the square size
$glyphSize = 0.66   # glyph width, as a fraction of the icon size

$dir = $PSScriptRoot
$bytes = [IO.File]::ReadAllBytes((Join-Path $dir 'source\team_KX4_icon.ico'))

# --- Take the 256 px frame (a PNG entry) from the source icon. ---
$count = [BitConverter]::ToUInt16($bytes, 4)
$big = $null
for ($i = 0; $i -lt $count; $i++) {
    $o = 6 + 16 * $i
    if ($bytes[$o] -ne 0) { continue }   # width byte 0 means 256 px
    $len = [int][BitConverter]::ToUInt32($bytes, $o + 8)
    $off = [int][BitConverter]::ToUInt32($bytes, $o + 12)
    $big = New-Object Drawing.Bitmap ([IO.MemoryStream]::new($bytes, $off, $len))
}
if (-not $big) { throw 'no 256 px frame in source icon' }

# --- People are transparent cut-outs in a black disc. Use the inverse alpha
# inside the disc (clear of its anti-aliased rim) as the glyph mask. ---
$n = $big.Width; $c = ($n - 1) / 2.0; $inner = 0.9 * $n / 2.0
$mask = New-Object Drawing.Bitmap $n, $n, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
$minX = $n; $minY = $n; $maxX = 0; $maxY = 0
for ($y = 0; $y -lt $n; $y++) {
    for ($x = 0; $x -lt $n; $x++) {
        $dx = $x - $c; $dy = $y - $c
        if (($dx * $dx + $dy * $dy) -gt ($inner * $inner)) { continue }
        $a = 255 - $big.GetPixel($x, $y).A
        if ($a -le 8) { continue }
        $mask.SetPixel($x, $y, [Drawing.Color]::FromArgb($a, $fg.R, $fg.G, $fg.B))
        if ($x -lt $minX) { $minX = $x }; if ($x -gt $maxX) { $maxX = $x }
        if ($y -lt $minY) { $minY = $y }; if ($y -gt $maxY) { $maxY = $y }
    }
}
$crop = New-Object Drawing.Rectangle $minX, $minY, ($maxX - $minX + 1), ($maxY - $minY + 1)

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

    # Glyph centered, scaled by width, aspect ratio kept.
    $gw = $size * $glyphSize
    $gh = $gw * $crop.Height / $crop.Width
    $dest = New-Object Drawing.RectangleF (($size - $gw) / 2), (($size - $gh) / 2), $gw, $gh
    $g.DrawImage($mask, $dest, $crop, [Drawing.GraphicsUnit]::Pixel)
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
