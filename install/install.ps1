# Magine CLI installer for Windows (PowerShell 5.1 or 7):
#
#   irm https://magine.cloud/install.ps1 | iex
#
# Downloads the native binary (the npm package @zeupiter/magine-cli-win32-<arch>), checks its
# sha512 against the npm registry, and puts it in %LOCALAPPDATA%\Programs\magine.
# No Node.js and no administrator rights needed; it never signs you in by itself.
#
#   $env:MAGINE_VERSION = '1.7.5'        install this version (default: latest)
#   $env:MAGINE_INSTALL_DIR = '<dir>'    where magine.exe goes
#   $env:MAGINE_NO_MODIFY_PATH = '1'     do not add that folder to your user PATH

& {
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  function Fail([string] $message) { throw "magine install: $message" }
  $color = -not $env:NO_COLOR -and -not [Console]::IsOutputRedirected
  # Say 'text' Magenta 'more text' Green ...: one line, each part in its color.
  function Say {
    for ($i = 0; $i -lt $args.Count; $i += 2) {
      $fg = if ($color -and $i + 1 -lt $args.Count -and $args[$i + 1]) { @{ ForegroundColor = $args[$i + 1] } } else { @{} }
      Write-Host $args[$i] -NoNewline @fg
    }
    Write-Host ''
  }

  $registry = if ($env:MAGINE_NPM_REGISTRY) { $env:MAGINE_NPM_REGISTRY.TrimEnd('/') } else { 'https://registry.npmjs.org' }
  if ($registry -notmatch '^(https://|http://(localhost|127\.0\.0\.1):)') { Fail 'MAGINE_NPM_REGISTRY must start with https://' }
  $version = if ($env:MAGINE_VERSION) { $env:MAGINE_VERSION } else { 'latest' }
  $dir = if ($env:MAGINE_INSTALL_DIR) { $env:MAGINE_INSTALL_DIR.TrimEnd('\') } else { Join-Path $env:LOCALAPPDATA 'Programs\magine' }
  if (-not [IO.Path]::IsPathRooted($dir) -or $dir.Contains(';')) { Fail 'MAGINE_INSTALL_DIR must be an absolute path without ;' }

  # 32-bit PowerShell on 64-bit Windows reports x86; PROCESSOR_ARCHITEW6432 has the real one.
  $cpu = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
  $arch = switch ($cpu) { 'AMD64' { 'x64' } 'ARM64' { 'arm64' } default { Fail "there is no Magine CLI build for $cpu yet" } }
  if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) { Fail 'this installer needs tar.exe (Windows 10 1803 or later). Or use: npm install -g magine' }
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

  $pkg = "@zeupiter/magine-cli-win32-$arch"
  Say '  /\_/\   ' Magenta 'Magine CLI installer' White
  Say ' ( o.o )  ' Magenta "Looking up $pkg@$version"
  Say '  > ^ <' Magenta
  try {
    $meta = Invoke-RestMethod -UseBasicParsing -Uri "$registry/@zeupiter%2fmagine-cli-win32-$arch/$version"
  } catch {
    Fail "could not reach $registry (is $pkg@$version published?): $($_.Exception.Message)"
  }
  $tarball = [string] $meta.dist.tarball
  $integrity = [string] $meta.dist.integrity
  if (-not $tarball -or -not $integrity) { Fail "the registry answered without a download for $pkg" }
  if (-not $tarball.StartsWith("$registry/")) { Fail "the download is not on $registry; refusing it" }
  if (-not $integrity.StartsWith('sha512-')) { Fail "the registry gave no sha512 checksum for $pkg" }

  $work = Join-Path ([IO.Path]::GetTempPath()) ('magine-' + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $work | Out-Null
  try {
    $tgz = Join-Path $work 'package.tgz'
    Say "Downloading magine $($meta.version) for windows/$arch..." DarkGray
    Invoke-WebRequest -UseBasicParsing -Uri $tarball -OutFile $tgz
    $sha = [Security.Cryptography.SHA512]::Create()
    $stream = [IO.File]::OpenRead($tgz)
    try { $hash = [Convert]::ToBase64String($sha.ComputeHash($stream)) } finally { $stream.Dispose(); $sha.Dispose() }
    if ("sha512-$hash" -ne $integrity) { Fail 'the download does not match its checksum; nothing was installed' }
    & tar.exe -xzf $tgz -C $work
    if ($LASTEXITCODE -ne 0) { Fail 'could not unpack the download' }
    $exe = Join-Path $work 'package\magine.exe'
    if (-not (Test-Path $exe)) { Fail 'the package has no magine.exe' }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $target = Join-Path $dir 'magine.exe'
    Copy-Item $exe "$target.new" -Force
    # A running magine.exe cannot be overwritten, but it can be renamed out of the way.
    if (Test-Path $target) {
      Remove-Item "$target.old" -Force -ErrorAction SilentlyContinue
      Move-Item $target "$target.old" -Force
    }
    Move-Item "$target.new" $target -Force
    Remove-Item "$target.old" -Force -ErrorAction SilentlyContinue
    $installed = (& $target --version 2>$null | Out-String).Trim()
    if (-not $installed) { Fail 'the installed magine.exe does not run on this computer' }
  } finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
  }

  # Read and write the raw registry value so entries like %USERPROFILE%\bin stay as they are.
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
  try {
    $userPath = [string] $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $parts = @($userPath -split ';' | Where-Object { $_ })
    $known = @($parts + ($env:Path -split ';') | ForEach-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') })
    $onPath = $known -contains $dir
    $addPath = -not $onPath -and $env:MAGINE_NO_MODIFY_PATH -ne '1'
    if ($addPath) {
      $kind = if ($userPath.Contains('%')) { [Microsoft.Win32.RegistryValueKind]::ExpandString } else { [Microsoft.Win32.RegistryValueKind]::String }
      $key.SetValue('Path', ((@($dir) + $parts) -join ';'), $kind)
      # Setting any user variable makes Windows tell open programs that PATH changed.
      [Environment]::SetEnvironmentVariable('MAGINE_INSTALL_REFRESH', $null, 'User')
      $env:Path = "$dir;$env:Path"
      Say "Added $dir to your user PATH."
    }
  } finally {
    $key.Dispose()
  }

  Say ''
  Say '  /\_/\   ' Magenta "Magine CLI $installed is installed" Green ": $target"
  Say ' ( o.o )  ' Magenta 'Next: ' $null 'magine login' White
  Say '  > ^ <   ' Magenta 'Docs: ' $null 'https://magine.cloud/docs#cli' Cyan
  if ($addPath) { Say 'Open a new terminal to use it everywhere.' DarkGray }
}
