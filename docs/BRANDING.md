# Branding: banner and icon

Copy a prompt below into an image generator (Midjourney, DALL·E / ChatGPT, Ideogram, Adobe Firefly, Flux, Stable Diffusion…). Generate several, pick the best, then follow "Using the result".

**The name:** **LanLM** reads "Lan-L-M": a language model (LM) that lives on your **LAN**. The idea to show is *several ordinary PCs on one local network that together run one AI*. Keep it simple, flat and friendly: the image must still read clearly at 16×16 pixels.

**Brand colours** (the same ones the app uses):

| Role | Colour |
| --- | --- |
| Primary blue | `#2f6df6` |
| Warm orange (worker PC) | `#e0762b` |
| Green (online / success) | `#1f9254` |
| Light background | `#f6f6f4` |
| Ink (text) | `#1f1f1d` |

---

## 1. Repository banner (README header and GitHub social preview)

**Size:** 1280×640 px (2:1). Most generators cannot output this exactly: generate 2:1 or 16:9 and crop.

> **Tip:** AI generators spell text badly. The prompts below ask for **no text** and keep the left third empty. Add the title "LanLM" and the tagline yourself, or send the image to the maintainers and we will place them with the correct font.

### Prompt (light version)

```
Wide hero banner illustration for an open-source desktop app called "LanLM", a tool that runs AI language models locally and links several ordinary computers on the same local network (a home or office LAN) so they work together as one. Clean, modern, minimal flat vector style with soft depth and gentle shadows. A small cluster of three or four stylised computers (a laptop, a desktop monitor and a compact mini PC) drawn as soft rounded rectangles, all connected by thin glowing lines through a small round router-like hub in the middle, with a rounded chat bubble containing a small spark symbol floating above the hub. Tiny light pulses travel along the connecting lines to show data flowing between the machines. Colour palette: warm off-white background (#f6f6f4) with a soft blue gradient glow (#2f6df6), small accents of warm orange (#e0762b) and green (#1f9254). Composition: all artwork on the right two-thirds of the image, large clean empty space on the left third for a title. Calm, trustworthy, friendly tech mood. 2:1 aspect ratio, ultra sharp, high resolution. No text, no letters, no logos, no watermark, no people.
```

### Prompt (dark version)

```
Wide hero banner illustration for an open-source desktop app called "LanLM", which runs AI language models locally and links several ordinary computers on the same local network so they work together as one. Clean, modern, minimal flat vector style with subtle glow. Three or four stylised computers (laptop, monitor, mini PC) as soft rounded rectangles, joined by thin glowing lines through a small round hub in the middle, with a rounded chat bubble containing a small spark floating above the hub. Tiny light pulses travel along the lines. Dark charcoal background (#262624) with a soft electric-blue glow (#6b97ff), small accents of orange (#f0924e) and green (#52c585). Artwork on the right two-thirds, big empty space on the left third for a title. Calm, premium, trustworthy mood. 2:1 aspect ratio, ultra sharp, high resolution. No text, no letters, no logos, no watermark, no people.
```

### Negative prompt (for generators that support one)

```
text, letters, words, typography, watermark, logo, signature, people, faces, hands, clutter, photo-realistic, 3d render of a server room, cables everywhere, neon cyberpunk, busy background, low contrast, blurry, distorted screens
```

**Midjourney:** add `--ar 2:1 --style raw --v 7` (use the newest version). **DALL·E / ChatGPT:** choose the widest size (1792×1024) and crop to 2:1.

---

## 2. App icon

**Size:** 1024×1024 px, square, PNG. It becomes the Windows app icon, the installer icon and the taskbar icon, so it must be simple, bold and readable when tiny.

### Prompt A: an "L" built from linked nodes (recommended)

```
App icon for a desktop app called LanLM, flat vector style, centred on a rounded-square tile with a smooth blue gradient (#2f6df6 to #1f4fd8). In the centre, a bold white capital letter "L" formed from thin straight connected lines and three small round nodes at its ends and corner, like a tiny network graph, with the node at the end of the horizontal stroke accented in warm orange (#e0762b). Very simple, high contrast, large shapes, generous empty margin around the symbol (about 15%), soft subtle inner highlight, no gradients inside the white symbol, no shadows outside the tile. Must stay clear and recognisable at 16x16 pixels. Square 1:1, 1024x1024. No other text, no watermark.
```

### Prompt B: computers linked on a LAN

```
App icon, flat vector, rounded-square tile in solid blue (#2f6df6). A white geometric symbol: three small rounded squares (suggesting three computers) arranged in a triangle and joined by thin white lines meeting at a small central dot (a network hub), with the top square tinted warm orange (#e0762b). Minimal, bold, symmetrical, large shapes, 15% margin, no tiny details, crisp edges. Looks good at 16x16 pixels. Square 1:1, 1024x1024. No text, no watermark.
```

### Prompt C: chat bubble with a small network inside

```
App icon, flat vector, rounded-square tile with a blue gradient (#2f6df6). A large white rounded chat bubble in the centre, and inside it a small network of three connected nodes (one orange #e0762b, two blue). Minimal, friendly, bold shapes, high contrast, 15% margin, crisp edges, readable at 16x16 pixels. Square 1:1, 1024x1024. No text, no watermark.
```

### Negative prompt

```
text, letters other than a single L, watermark, photo, 3d, realistic, gradients inside the symbol, tiny details, thin hairlines, busy background, shadows outside the tile, blurry, low contrast
```

---

## Current assets

| File | What it is |
| --- | --- |
| `app-icon.png` | The app icon: 1024×1024, transparent rounded-square tile. The source for every Windows icon size. |
| `src-tauri/icons/` | Generated from `app-icon.png` (do not edit by hand). |
| `docs/images/banner.png` | README header banner (1774×887). |
| `docs/images/social-preview.jpg` | The banner at 1280×640 for GitHub's social preview (under 1 MB). |
| `docs/images/source/logo-original.png` | The icon as generated (on a black background), kept in case it needs to be cut out again. |

## Using a new image

**Icon**

1. Save the new icon as a square PNG, ideally with a transparent background. If the generator gives you a tile on a solid black background, cut it out first (the black must become transparent, or Windows shows black corners).
2. Save it as `app-icon.png` in the repository root (1024×1024).
3. Regenerate every size the Windows app needs:
   ```bash
   npx tauri icon app-icon.png
   ```
   Then delete the unneeded mobile folders that command creates: `src-tauri/icons/android` and `src-tauri/icons/ios`.
4. Run `npm run verify` and commit.

**Banner**

1. Save as `docs/images/banner.png` (2:1, for example 1774×887 or 1280×640).
2. Make the 1280×640 social preview from it, saved as `docs/images/social-preview.jpg` and kept under 1 MB.
3. Upload that file under Settings → General → _Social preview_.

**Checks before you keep an image**

- [ ] Reads clearly when shrunk to 32 px (icon) or 640 px wide (banner)
- [ ] No stray or misspelled text, no watermark
- [ ] You have the rights to use it (check your generator's terms) and it contains no third-party logos
