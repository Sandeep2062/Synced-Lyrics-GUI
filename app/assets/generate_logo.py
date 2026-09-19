"""Script to generate a minimalist, vector-sharp audio waveform logo."""
import os
from PIL import Image, ImageDraw

def generate_minimalist_logo(output_png: str, output_ico: str):
    # Render at 4x resolution (2048x2048) for ultra-sharp anti-aliased downscaling
    SUPER_SIZE = 2048
    canvas = Image.new("RGBA", (SUPER_SIZE, SUPER_SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)

    # Minimalist design: Modern 5-bar rhythm equalizer wave with subtle gradient
    # Color palette: Gradient from Electric Violet (#8B5CF6) to Cyber Cyan (#06B6D4)
    colors = [
        (139, 92, 246, 255),   # #8B5CF6
        (124, 58, 237, 255),   # #7C3AED
        (99, 102, 241, 255),   # #6366F1
        (14, 165, 233, 255),   # #0EA5E9
        (6, 182, 212, 255),    # #06B6D4
    ]

    # Bar proportions (heights relative to center)
    bar_heights = [0.45, 0.78, 1.0, 0.70, 0.40]
    total_bars = len(bar_heights)
    
    # Dimensions in 2048 canvas
    bar_width = 180
    gap = 110
    total_w = (total_bars * bar_width) + ((total_bars - 1) * gap)
    start_x = (SUPER_SIZE - total_w) // 2
    max_h = 1350
    center_y = SUPER_SIZE // 2

    for i in range(total_bars):
        bx = start_x + i * (bar_width + gap)
        bh = int(max_h * bar_heights[i])
        by0 = center_y - (bh // 2)
        by1 = center_y + (bh // 2)
        radius = bar_width // 2
        color = colors[i]
        
        # Draw clean capsule
        draw.rounded_rectangle([bx, by0, bx + bar_width, by1], radius=radius, fill=color)

    # Downsample to 512x512 with LANCZOS
    final_512 = canvas.resize((512, 512), Image.Resampling.LANCZOS)
    final_512.save(output_png, format="PNG")
    print(f"Saved {output_png} (512x512)")

    # Save multi-size ICO
    ico_sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (24, 24), (16, 16)]
    final_512.save(output_ico, format="ICO", sizes=ico_sizes)
    print(f"Saved {output_ico} with multi-resolution icon sizes")

if __name__ == "__main__":
    assets_dir = os.path.dirname(os.path.abspath(__file__))
    png_path = os.path.join(assets_dir, "icon.png")
    ico_path = os.path.join(assets_dir, "icon.ico")
    generate_minimalist_logo(png_path, ico_path)
