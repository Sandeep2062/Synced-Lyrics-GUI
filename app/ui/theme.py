"""LRCGET-inspired pure-black color palette and theme constants."""

COLORS = {
    'bg_primary': '#0A0A0A',      # Main app background (neutral-950)
    'bg_secondary': '#121212',    # Row and card background
    'bg_tertiary': '#18181B',     # Pill and card inner background
    'bg_toolbar': '#171717',      # Header & titlebar (neutral-900)
    'bg_surface': '#141414',      # Modal / drawer background
    'bg_hover': '#1C1C1C',        # Hover state
    'bg_selected': '#242424',     # Selected state
    'bg_input': '#1A1A1E',        # Search & input background
    'bg_button': '#262626',       # Normal button background (neutral-800)
    'bg_button_hover': '#333333',
    'border': '#262626',
    'border_light': '#333333',
    'slider_rail': '#333338',
    'accent': '#F35697',          # LRCGET signature rose (hoa-1100)
    'accent_hover': '#FB73A4',
    'accent_active': '#DC4089',
    'accent_dark': '#A82C6C',
    'text_primary': '#FFFFFF',
    'text_secondary': '#A3A3A3',  # neutral-400
    'text_muted': '#737373',      # neutral-500
    'text_dim': '#4A4A4A',
    
    # Status Pill Backgrounds & Foregrounds (exact LRCGET badges)
    'pill_synced_bg': '#0A3018',
    'pill_synced_fg': '#4ADE80',
    'pill_plain_bg': '#1E293B',
    'pill_plain_fg': '#94A3B8',
    'pill_missing_bg': '#3A1212',
    'pill_missing_fg': '#F87171',
    'pill_suspicious_bg': '#3A240E',
    'pill_suspicious_fg': '#FBBF24',
    
    # Utility colors
    'success': '#4ADE80',
    'warning': '#FBBF24',
    'error': '#F87171',
    'info': '#38BDF8',
}

FONTS = {
    'heading': ('Segoe UI', 16, 'bold'),
    'subheading': ('Segoe UI', 13, 'bold'),
    'body': ('Segoe UI', 11),
    'body_bold': ('Segoe UI', 11, 'bold'),
    'small': ('Segoe UI', 9),
    'small_bold': ('Segoe UI', 9, 'bold'),
    'mono': ('Consolas', 10),
    'lyrics': ('Segoe UI', 15),
    'lyrics_active': ('Segoe UI', 17, 'bold'),
    'tab': ('Segoe UI', 12, 'bold'),
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
    'not_found': '#737373',
    'skipped': '#525252',
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
