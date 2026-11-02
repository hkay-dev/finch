---
title: Pasting images into a Markdown file
feature: paste-images
covers:
  - src/paste_image.rs
  - src/panes.rs
since: 2026-11-01
---

In a Markdown file you have saved, paste a screenshot or any copied image and Finch saves it beside the file and puts a link to it in the text.

## Paste an image

1. Copy an image, for example a screenshot.
2. Put the caret where the picture should go in a saved Markdown file.
3. Press Command-V.

Finch makes an `assets` folder next to the file if there isn't one, saves the image there, and types `![](assets/name.png)` at the caret once the image is saved. A PNG, JPEG, GIF, WebP or SVG image is saved as it is, keeping its type (a JPEG becomes `name.jpg`); any other kind, such as a screenshot, is saved as a PNG. The new link lights up for a moment as it lands. The preview shows the picture.

## Names

The image is named after the heading the caret is under, so a paste under "Getting started" is saved as `getting-started.png`. With no heading above the caret it is named `image-` and today's date. If that name is taken, the next picture gets `-2`, then `-3`. Finch never replaces an image that is already there.

## Drop an image file

Drag an image file from Finder onto the Markdown editor. Finch copies it into the `assets` folder, keeping its type (a `.jpg` stays a `.jpg`), and types its link at the caret. Dropping files on a tab bar or on a pane's edge opens them as before.

## Paste image files copied in Finder

Copy one or more image files in Finder, put the caret in a saved Markdown file and press Command-V. Finch adds them the way a drop does: each is copied into the `assets` folder, keeping its type, and its link goes in at the caret. If any copied file isn't an image, the file names are pasted as text.

## Undo

Undo takes the link out as one step. The image stays in the folder, so Redo brings the link back to a picture that is still there.

## When it doesn't apply

- An Untitled page, or a file that isn't Markdown, pastes as it always did. Save the page as a Markdown file first.
- When what you copied has text in it, the text is pasted, not the picture.
- If the image can't be saved, Finch says so and the text is left alone.
