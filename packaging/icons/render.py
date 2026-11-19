"""Export app.svg. Run render.sh from any directory."""

from pathlib import Path
import shutil
import subprocess
from tempfile import TemporaryDirectory
import xml.etree.ElementTree as ET

from PIL import Image

icons = Path(__file__).resolve().parent
mobile = icons.parents[1] / "apps/mobile"
svg = icons / "app.svg"
rsvg = shutil.which("rsvg-convert")
inkscape = shutil.which("inkscape")
if not (rsvg or inkscape):
    raise SystemExit("Install librsvg or Inkscape to render app.svg.")


def render(source, size, dest):
    if rsvg:
        cmd = [rsvg, "-w", str(size), "-h", str(size), "-o", dest, source]
    else:
        cmd = [inkscape, source, "-w", str(size), "-h", str(size), "-o", dest]
    subprocess.run(cmd, check=True)


def android_foreground(root):
    # A 440-unit viewport keeps the capybara and its page in the adaptive icon's safe circle.
    android = "http://schemas.android.com/apk/res/android"
    aapt = "http://schemas.android.com/aapt"
    ET.register_namespace("android", android)
    ET.register_namespace("aapt", aapt)

    def attrs(**values):
        return {f"{{{android}}}{k}": v for k, v in values.items()}

    vector = ET.Element("vector", attrs(width="108dp", height="108dp",
                                        viewportWidth="440", viewportHeight="440"))
    group = ET.SubElement(vector, "group", attrs(translateX="92", translateY="92"))
    for face in root.find(".//*[@id='monogram']"):
        geometry = " ".join(face[0].attrib["d"].split())
        path = ET.SubElement(group, "path", attrs(pathData=geometry))
        fill = ET.SubElement(path, f"{{{aapt}}}attr", name="android:fillColor")
        gradient_id = face.attrib["fill"].removeprefix("url(#").removesuffix(")")
        paint = root.find(f".//*[@id='{gradient_id}']")
        grad = ET.SubElement(fill, "gradient", attrs(type="linear", startX=paint.get("x1"),
                             startY=paint.get("y1"), endX=paint.get("x2"), endY=paint.get("y2")))
        for stop in paint:
            ET.SubElement(grad, "item", attrs(offset=stop.attrib["offset"],
                                              color=stop.attrib["stop-color"]))
    ET.indent(vector, space="    ")
    dest = mobile / "android/res/drawable/ic_launcher_foreground.xml"
    dest.write_text('<!-- Generated from packaging/icons/app.svg by render.sh. -->\n'
                    + ET.tostring(vector, encoding="unicode") + "\n", encoding="utf-8")


def android_monochrome(root):
    # The themed icon is drawn in one colour from the alpha alone, so the face and the text are
    # holes, and the head stops short of the page: only the paws join the two.
    android = "http://schemas.android.com/apk/res/android"

    def attrs(**values):
        return {f"{{{android}}}{k}": v for k, v in values.items()}

    def geometry(*ids):
        return " ".join(" ".join(root.find(f".//*[@id='{i}']").attrib["d"].split()) for i in ids)

    vector = ET.Element("vector", attrs(width="108dp", height="108dp",
                                        viewportWidth="440", viewportHeight="440"))
    group = ET.SubElement(vector, "group", attrs(translateX="92", translateY="92"))
    above = ET.SubElement(group, "group")
    ET.SubElement(above, "clip-path", attrs(pathData="M 0 0 H 256 V 138 H 0 Z"))
    shapes = ((above, ("head", "face")), (above, ("markings",)), (above, ("orange-fruit",)),
              (above, ("orange-leaf",)), (group, ("page", "text")), (group, ("paws",)))
    for parent, ids in shapes:
        ET.SubElement(parent, "path", attrs(pathData=geometry(*ids), fillColor="#000000",
                                            fillType="evenOdd"))
    ET.indent(vector, space="    ")
    dest = mobile / "android/res/drawable/ic_launcher_monochrome.xml"
    dest.write_text('<!-- Generated from packaging/icons/app.svg by render.sh. -->\n'
                    + ET.tostring(vector, encoding="unicode") + "\n", encoding="utf-8")


sizes = (16, 24, 32, 48, 64, 128, 256, 512, 1024)
images = {}
for size in sizes:
    png = icons / f"app-{size}.png"
    render(svg, size, png)
    with Image.open(png) as image:
        images[size] = image.copy()

images[256].save(icons / "app.ico", sizes=[(s, s) for s in sizes if s <= 256],
                 append_images=list(images.values()))
images[1024].save(icons / "app.icns", append_images=list(images.values()))

ET.register_namespace("", "http://www.w3.org/2000/svg")
ET.register_namespace("xlink", "http://www.w3.org/1999/xlink")
tree = ET.parse(svg)
root = tree.getroot()
android_foreground(root)
android_monochrome(root)

# iOS supplies the corner mask. Render the same artwork and background to all four edges.
root.set("viewBox", "0 0 256 256")
del root.find(".//*[@id='artwork']").attrib["clip-path"]
with TemporaryDirectory() as tmp:
    square = Path(tmp) / "square.svg"
    png = Path(tmp) / "square.png"
    tree.write(square, encoding="utf-8")
    render(square, 1024, png)
    with Image.open(png) as image:
        if image.convert("RGBA").getchannel("A").getextrema() != (255, 255):
            raise ValueError("The iOS app icon must be opaque to every edge.")
        image.convert("RGB").save(mobile / "ios/Assets.xcassets/AppIcon.appiconset/AppIcon.png")
