"""Check the committed launcher assets with python3 -m unittest discover -s packaging/icons."""

from pathlib import Path
import unittest
import xml.etree.ElementTree as ET

from PIL import Image


ICONS = Path(__file__).resolve().parent
MOBILE = ICONS.parents[1] / "apps/mobile"


class LauncherIcons(unittest.TestCase):
    def test_svg_artwork_is_self_contained(self):
        svg = ET.parse(ICONS / "app.svg")
        self.assertIsNone(svg.find(".//{*}image"))
        ids = {element.get("id") for element in svg.iter() if "id" in element.attrib}
        for use in svg.iterfind(".//{*}use"):
            reference = use.attrib["{http://www.w3.org/1999/xlink}href"]
            self.assertTrue(reference.startswith("#"))
            self.assertIn(reference[1:], ids)

    def test_android_uses_the_svg_faces_in_paint_order(self):
        svg = ET.parse(ICONS / "app.svg")
        faces = svg.find(".//*[@id='monogram']")
        expected = [" ".join(face[0].attrib["d"].split()) for face in faces]

        vector = ET.parse(MOBILE / "android/res/drawable/ic_launcher_foreground.xml")
        android = "{http://schemas.android.com/apk/res/android}"
        actual = [path.attrib[android + "pathData"] for path in vector.iter("path")]
        self.assertEqual(actual, expected)

    def test_android_themed_icon_cuts_out_the_face_and_the_text(self):
        svg = ET.parse(ICONS / "app.svg")

        def geometry(*ids):
            return " ".join(" ".join(svg.find(f".//*[@id='{i}']").attrib["d"].split())
                            for i in ids)

        vector = ET.parse(MOBILE / "android/res/drawable/ic_launcher_monochrome.xml")
        android = "{http://schemas.android.com/apk/res/android}"
        paths = list(vector.iter("path"))
        actual = [path.attrib[android + "pathData"] for path in paths]
        self.assertEqual(actual, [geometry("head", "face"), geometry("markings"),
                                  geometry("orange-fruit"), geometry("orange-leaf"),
                                  geometry("page", "text"), geometry("paws")])
        for path in paths:
            self.assertEqual(path.attrib[android + "fillType"], "evenOdd")
        icon = (MOBILE / "android/res/mipmap-anydpi-v26/ic_launcher.xml").read_text()
        self.assertIn('<monochrome android:drawable="@drawable/ic_launcher_monochrome" />', icon)

    def test_launcher_sizes_and_masks(self):
        for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
            with self.subTest(size=size), Image.open(ICONS / f"app-{size}.png") as png:
                self.assertEqual(png.size, (size, size))
                self.assertEqual(png.getchannel("A").getpixel((0, 0)), 0)
        with Image.open(ICONS / "app.ico") as ico:
            self.assertEqual(ico.ico.sizes(), {(s, s) for s in (16, 24, 32, 48, 64, 128, 256)})
        with Image.open(MOBILE / "ios/Assets.xcassets/AppIcon.appiconset/AppIcon.png") as ios:
            self.assertEqual(ios.size, (1024, 1024))
            self.assertEqual(ios.mode, "RGB")
