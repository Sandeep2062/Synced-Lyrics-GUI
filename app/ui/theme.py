"""Modern Electric Violet & Midnight Obsidian color palette and theme constants."""

COLORS = {
    'bg_primary': '#090A0F',      # Main app background (Midnight Obsidian)
    'bg_secondary': '#12141D',    # Row and card background
    'bg_tertiary': '#181B26',     # Pill and card inner background
    'bg_toolbar': '#131622',      # Header & titlebar
    'bg_surface': '#1A1D2C',      # Modal / drawer background
    'bg_hover': '#1E2235',        # Hover state
    'bg_selected': '#282D46',     # Selected state
    'bg_input': '#121520',        # Search & input background
    'bg_button': '#202436',       # Normal button background
    'bg_button_hover': '#2C324B',
    'border': '#23273A',
    'border_light': '#313752',
    'slider_rail': '#262B40',
    
    # Creative Electric Violet & Cyber Cyan Accents
    'accent': '#8B5CF6',          # Vibrant Electric Violet
    'accent_hover': '#A78BFA',
    'accent_active': '#7C3AED',
    'accent_dark': '#5B21B6',
    'accent_cyan': '#06B6D4',     # Cyber Cyan secondary accent
    'accent_cyan_hover': '#22D3EE',
    
    'text_primary': '#FFFFFF',
    'text_secondary': '#94A3B8',  # Slate-400
    'text_muted': '#64748B',      # Slate-500
    'text_dim': '#475569',
    
    # Status Pill Backgrounds & Foregrounds
    'pill_synced_bg': '#064E3B',
    'pill_synced_fg': '#34D399',  # Crisp Emerald Mint
    'pill_plain_bg': '#0C4A6E',
    'pill_plain_fg': '#38BDF8',  # Sky Blue
    'pill_missing_bg': '#4C0519',
    'pill_missing_fg': '#FB7185', # Coral Rose
    'pill_suspicious_bg': '#451A03',
    'pill_suspicious_fg': '#FBBF24', # Amber Gold
    
    # Utility colors
    'success': '#34D399',
    'warning': '#FBBF24',
    'error': '#FB7185',
    'info': '#38BDF8',
}

FONTS = {
    'heading': ('Segoe UI', 15, 'bold'),
    'subheading': ('Segoe UI', 13, 'bold'),
    'body': ('Segoe UI', 11),
    'body_bold': ('Segoe UI', 11, 'bold'),
    'small': ('Segoe UI', 9),
    'small_bold': ('Segoe UI', 9, 'bold'),
    'mono': ('Consolas', 10),
    'lyrics': ('Segoe UI', 15),
    'lyrics_active': ('Segoe UI', 17, 'bold'),
    'tab': ('Segoe UI', 11, 'bold'),
}

STATUS_COLORS = {
    'synced': COLORS['pill_synced_fg'],
    'plain': COLORS['pill_plain_fg'],
    'missing': COLORS['pill_missing_fg'],
    'suspicious': COLORS['pill_suspicious_fg'],
    'error': '#EF4444',
    'searching': '#38BDF8',
    'found_synced': COLORS['pill_synced_fg'],
    'found_plain': COLORS['pill_plain_fg'],
    'not_found': '#64748B',
    'skipped': '#475569',
    'rate_limited': '#FBBF24',
}

STATUS_ICONS = {
    'synced': 'Synced',
    'plain': 'Plain',
    'missing': 'Missing',
    'suspicious': 'Suspicious',
    'found_synced': 'Synced',
    'found_plain': 'Plain',
    'not_found': 'Not Found',
    'searching': 'Searching',
    'skipped': 'Skipped',
    'rate_limited': 'Rate Limited',
    'error': 'Error',
}

def get_status_colors(status: str) -> tuple[str, str]:
    """Returns (bg_color, fg_color) for a given status."""
    st = (status or 'missing').lower()
    if st == 'synced':
        return COLORS['pill_synced_bg'], COLORS['pill_synced_fg']
    elif st == 'plain':
        return COLORS['pill_plain_bg'], COLORS['pill_plain_fg']
    elif st == 'suspicious':
        return COLORS['pill_suspicious_bg'], COLORS['pill_suspicious_fg']
    else:
        return COLORS['pill_missing_bg'], COLORS['pill_missing_fg']

def set_accent_color(hex_color: str):
    """Dynamically updates the app accent color."""
    COLORS['accent'] = hex_color
