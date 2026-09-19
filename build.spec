# -*- mode: python ; coding: utf-8 -*-
import os
import customtkinter

block_cipher = None

a = Analysis(
    ['app/__main__.py'],
    pathex=[],
    binaries=[],
    datas=[
        (os.path.dirname(customtkinter.__file__), 'customtkinter'),
        ('app/assets', 'app/assets'),
    ],
    hiddenimports=[
        'customtkinter',
        'syncedlyrics',
        'syncedlyrics.providers',
        'mutagen',
        'mutagen.flac',
        'mutagen.mp3',
        'mutagen.mp4',
        'mutagen.oggopus',
        'mutagen.oggvorbis',
        'mutagen.wave',
        'pygame',
        'PIL',
        'PIL.Image',
        'PIL.ImageDraw',
        'requests',
        'packaging',
        'packaging.version',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name='SyncedLyricsGUI',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon='app/assets/icon.ico',
)
