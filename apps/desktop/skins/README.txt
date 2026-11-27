Super Media Player skins
========================

A skin recolours one of the player's views. It is a plain text file ending
in ".skin" in this folder. The easiest way to make one is in the player:
View > Themes and colours, change the colours, Save as my theme.

After you add or change a file by hand, choose View > Skin > Reload skins
(or Settings > Your own skins > Reload). It then shows up in View > Skin
and in Settings > Skin.

    # Lines starting with # are comments.
    name   = Silver deck – Night shift     (shown in the menus)
    layout = silver                        (player, compact, classic, silver,
                                            or any: a theme for the media
                                            player and compact views)
    author = Your name                     (optional)

    # Then colours: a name from the layout's list below, and #rgb, #rrggbb,
    # #rrggbbaa (the last two digits are the opacity) or "transparent".
    case-1 = #3a3f47
    led    = #39e0ff

    # "theme." colours change the dialogs, buttons, popups and playlist text
    # too, in any layout.
    theme.accent = #39e0ff

Anything you leave out keeps the layout's own colour, so a skin can be two
lines long. Colours ending in -1, -2, -3 are the stops of one gradient,
from top (or centre) to bottom (or edge). A skin with a mistake still
loads; the player lists what it skipped when you reload.

The colour names for each layout, with their defaults, are in the
built-in skins: see SKINS.md in the source code, or copy one of the
examples in the "examples" folder next to this file and change it.
