"""生成 embhub 的图标（.ico / .png）。

用 DSH 捆绑的 Python + Pillow 运行，不额外安装依赖：
    <bundled python> scripts/make_icon.py

为什么需要它：tauri-build 在 Windows 上要用 icons/icon.ico 生成资源文件，
缺了会直接编译失败（实测报错：required for generating a Windows Resource file）。
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

OUT_DIR = Path(__file__).resolve().parent.parent / "crates" / "embhub-app" / "icons"

BG = (20, 22, 26, 255)  # 与 UI 背景一致的深色
ACCENT = (126, 231, 135, 255)  # 终端提示符的绿色
FRAME = (58, 66, 80, 255)

SIZE = 256


def load_font(px: int):
    for name in ("consola.ttf", "CascadiaMono.ttf", "lucon.ttf", "arial.ttf"):
        try:
            return ImageFont.truetype(name, px)
        except OSError:
            continue
    return ImageFont.load_default()


def build() -> Image.Image:
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # 圆角底
    d.rounded_rectangle([8, 8, SIZE - 8, SIZE - 8], radius=48, fill=BG, outline=FRAME, width=4)

    # 终端提示符 ">_"：一个尖括号加一条下划线，最简、缩到 16px 也认得出
    font = load_font(150)
    d.text((44, 36), ">", font=font, fill=ACCENT)
    d.rounded_rectangle([132, 158, 200, 174], radius=4, fill=ACCENT)

    return img


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    img = build()

    ico = OUT_DIR / "icon.ico"
    img.save(ico, format="ICO", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

    png = OUT_DIR / "icon.png"
    img.save(png, format="PNG")

    print(f"wrote {ico} ({ico.stat().st_size} bytes)")
    print(f"wrote {png} ({png.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
