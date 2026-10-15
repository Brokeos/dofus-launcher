"""Extract Texture2D/Sprite images from Dofus 3 Unity asset bundles.
Usage: python extract_sprites.py <bundle> <out_dir> [name_filter]"""
import sys, os, UnityPy
UnityPy.config.FALLBACK_UNITY_VERSION = "6000.3.16f1"

bundle, out = sys.argv[1], sys.argv[2]
flt = sys.argv[3].lower() if len(sys.argv) > 3 else ""
os.makedirs(out, exist_ok=True)
env = UnityPy.load(bundle)
n = 0
for obj in env.objects:
    if obj.type.name not in ("Sprite", "Texture2D"):
        continue
    data = obj.read()
    name = getattr(data, "m_Name", "") or f"obj{obj.path_id}"
    if flt and flt not in name.lower():
        continue
    path = os.path.join(out, f"{obj.type.name}_{name}.png")
    if os.path.exists(path):
        continue
    try:
        data.image.save(path)
        n += 1
    except Exception as e:  # unsupported formats
        print("skip", name, e)
print("extracted", n)
