# Downloads the portable conversion tools into tools\. Nothing is installed
# system-wide. Safe to rerun: finished tools are skipped.
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = 'Tls12'
$ProgressPreference = 'SilentlyContinue'

$tools = Join-Path $PSScriptRoot '..\tools' | Resolve-Path -ErrorAction SilentlyContinue
if (-not $tools) { $tools = (New-Item -ItemType Directory (Join-Path $PSScriptRoot '..\tools')).FullName }
$cache = Join-Path $tools '_downloads'
New-Item -ItemType Directory -Force $cache | Out-Null

function Fetch($name, $url) {
    $dir = Join-Path $cache $name
    New-Item -ItemType Directory -Force $dir | Out-Null
    $file = Join-Path $dir ([IO.Path]::GetFileName($url))
    if (-not (Test-Path $file)) {
        Write-Host "downloading $([IO.Path]::GetFileName($url))"
        Invoke-WebRequest $url -OutFile "$file.part" -UseBasicParsing
        Move-Item "$file.part" $file
    }
    $file
}

$sevenR = Fetch '7zr' 'https://github.com/ip7z/7zip/releases/download/26.05/7zr.exe'

function Unpack($archive, $target) {
    # 7zr only reads .7z, so it unpacks the full 7-Zip, which handles the rest.
    $unpacker = if (Test-Path "$tools\7zip\7z.exe") { "$tools\7zip\7z.exe" } else { $sevenR }
    New-Item -ItemType Directory -Force $target | Out-Null
    & $unpacker x $archive "-o$target" -y | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Remove-Item $target -Recurse -Force
        throw "failed to unpack $archive"
    }
}

if (-not (Test-Path "$tools\7zip\7z.exe")) {
    Unpack (Fetch '7zip' 'https://github.com/ip7z/7zip/releases/download/26.05/7z2605-x64.exe') "$tools\7zip"
}
if (-not (Test-Path "$tools\pandoc")) {
    Unpack (Fetch 'pandoc' 'https://github.com/jgm/pandoc/releases/download/3.13/pandoc-3.13-windows-x86_64.zip') "$tools\pandoc"
}
if (-not (Test-Path "$tools\magick")) {
    Unpack (Fetch 'magick' 'https://github.com/ImageMagick/ImageMagick/releases/download/7.1.2-35/ImageMagick-7.1.2-35-portable-Q16-HDRI-x64.7z') "$tools\magick"
}
if (-not (Test-Path "$tools\qpdf")) {
    Unpack (Fetch 'qpdf' 'https://github.com/qpdf/qpdf/releases/download/v12.4.2/qpdf-12.4.2-msvc64.zip') "$tools\qpdf"
}
if (-not (Test-Path "$tools\poppler")) {
    Unpack (Fetch 'poppler' 'https://github.com/oschwartz10612/poppler-windows/releases/download/v26.09.0-0/Release-26.09.0-0.zip') "$tools\poppler"
}
if (-not (Test-Path "$tools\ffmpeg\ffmpeg.exe")) {
    $bin = Split-Path (Get-Command ffmpeg).Source
    New-Item -ItemType Directory -Force "$tools\ffmpeg" | Out-Null
    Copy-Item "$bin\ffmpeg.exe", "$bin\ffprobe.exe" "$tools\ffmpeg"
}
if (-not (Test-Path "$tools\libreoffice")) {
    $msi = Fetch 'libreoffice' 'https://download.documentfoundation.org/libreoffice/stable/26.8.2/win/x86_64/LibreOffice_26.8.2_Win_x86-64.msi'
    # An administrative extract unpacks files only and registers nothing.
    Write-Host 'unpacking LibreOffice'
    $msiArgs = @('/a', "`"$msi`"", '/qn', "TARGETDIR=`"$tools\libreoffice`"")
    $run = Start-Process msiexec.exe -ArgumentList $msiArgs -Wait -PassThru
    if ($run.ExitCode -ne 0) { throw "LibreOffice unpack failed with exit code $($run.ExitCode)" }
}
'done'
Get-ChildItem $tools -Directory | Where-Object Name -ne '_downloads' | ForEach-Object {
    $mb = [math]::Round((Get-ChildItem $_.FullName -Recurse -File | Measure-Object Length -Sum).Sum / 1MB)
    "$($_.Name): $mb MB"
}
