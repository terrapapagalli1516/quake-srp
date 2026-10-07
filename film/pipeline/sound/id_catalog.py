"""What each of id's shareware sounds is: category and use. From the names and Quake's game code
(QuakeC's sound() calls, the client's temp entities and menu); '(by name)' marks a guess."""

MONSTER = {"soldier": "Grunt", "dog": "Rottweiler", "knight": "Knight", "hknight": "Death Knight",
           "ogre": "Ogre", "wizard": "Scrag", "zombie": "Zombie", "demon": "Fiend", "shambler": "Shambler",
           "boss1": "Chthon"}

WHAT = {
    # ambience: the level's looping sounds
    "ambience/buzz1": "electric buzz of a sparking light (ambient_light_buzz)",
    "ambience/comp1": "computer bleeps and hum (ambient_comp_hum)",
    "ambience/drip1": "dripping water (ambient_drip)",
    "ambience/drone6": "low machine drone (ambient_drone)",
    "ambience/fire1": "THE TORCH: the hiss of wall torches and flames (light_torch_small_walltorch, light_flame_*)",
    "ambience/fl_hum1": "fluorescent light hum (light_fluoro)",
    "ambience/hum1": "teleporter hum (trigger_teleport)",
    "ambience/suck1": "wind sucking through (ambient_suck_wind)",
    "ambience/swamp1": "swamp: frogs, bubbling (ambient_swamp1)",
    "ambience/swamp2": "swamp: frogs, bubbling (ambient_swamp2)",
    "ambience/thunder1": "thunder (ambient_thunder)",
    "ambience/water1": "flowing water: the engine's ambient for leaves in water",
    "ambience/wind2": "wind: the engine's ambient for leaves that see the sky",
    "ambience/windfly": "a rush of wind (trigger_push, the wind tunnels)",
    # Chthon
    "boss1/death": "Chthon's death", "boss1/out1": "Chthon rising out of the lava",
    "boss1/pain": "Chthon hit by the lightning", "boss1/sight1": "Chthon's roar on waking",
    "boss1/throw": "Chthon throwing a lava ball",
    # buttons
    "buttons/airbut1": "button: steam metal", "buttons/switch02": "button: metallic click",
    "buttons/switch04": "button: in-out", "buttons/switch21": "button: wooden clunk",
    # Fiend
    "demon/ddeath": "Fiend's death", "demon/dhit2": "Fiend's claws hitting", "demon/djump": "Fiend leaping",
    "demon/dland2": "Fiend landing", "demon/dpain1": "Fiend's pain", "demon/idle1": "Fiend idle",
    "demon/sight2": "Fiend's sight",
    # Rottweiler
    "dog/dattack1": "Rottweiler's bite", "dog/ddeath": "Rottweiler's death", "dog/dpain1": "Rottweiler's pain",
    "dog/dsight": "Rottweiler's bark on sight", "dog/idle": "Rottweiler's growl",
    # doors (move = while moving, often looped; stop = when it arrives)
    "doors/airdoor1": "secret door (metal), moving", "doors/airdoor2": "secret door (metal), stopping",
    "doors/basesec1": "secret door (base), moving", "doors/basesec2": "secret door (base), stopping",
    "doors/basetry": "locked base door", "doors/baseuse": "base door opened with a key",
    "doors/ddoor1": "door (screechy metal), moving", "doors/ddoor2": "door (screechy metal), stopping",
    "doors/doormv1": "door (stone), moving", "doors/drclos4": "door (stone) closing; secret door stopping",
    "doors/hydro1": "door (base, hydraulic), moving", "doors/hydro2": "door (base, hydraulic), stopping",
    "doors/latch2": "secret door (medieval), its latch", "doors/medtry": "locked medieval door",
    "doors/meduse": "medieval door opened with a key", "doors/runetry": "locked runic door",
    "doors/runeuse": "runic door opened with a key", "doors/stndr1": "door (stone chain), moving",
    "doors/stndr2": "door (stone chain), stopping", "doors/winch2": "secret door (medieval), its winch",
    "hknight/hit": "a Death Knight's magic spike hitting (the client's TE_KNIGHTSPIKE)",
    # items
    "items/armor1": "armour pickup", "items/damage": "Quad Damage pickup", "items/damage2": "Quad Damage wearing off",
    "items/damage3": "firing with the Quad", "items/health1": "health pickup",
    "items/inv1": "Ring of Shadows pickup", "items/inv2": "Ring of Shadows wearing off",
    "items/inv3": "Ring of Shadows: breathing while invisible", "items/itembk2": "an item respawning (deathmatch)",
    "items/protect": "Pentagram of Protection pickup", "items/protect2": "Pentagram wearing off",
    "items/protect3": "a hit absorbed by the Pentagram", "items/r_item1": "small (rotten) health pickup",
    "items/r_item2": "Megahealth pickup", "items/suit": "Biosuit pickup", "items/suit2": "Biosuit wearing off",
    # Knight
    "knight/idle": "Knight idle", "knight/kdeath": "Knight's death", "knight/khurt": "Knight's pain",
    "knight/ksight": "Knight's sight", "knight/sword1": "Knight's sword swing", "knight/sword2": "Knight's sword swing",
    # misc: menu, messages, keys, teleports
    "misc/h2ohit1": "landing in water, a splash", "misc/medkey": "key pickup (medieval)",
    "misc/menu1": "MENU: cursor up/down", "misc/menu2": "MENU: enter, select", "misc/menu3": "MENU: slider or toggle changed",
    "misc/null": "silence (a placeholder)", "misc/outwater": "leaving water", "misc/power": "(by name) a power surge",
    "misc/r_tele1": "teleport arrival (one of five)", "misc/r_tele2": "teleport arrival (one of five)",
    "misc/r_tele3": "teleport arrival (one of five)", "misc/r_tele4": "teleport arrival (one of five)",
    "misc/r_tele5": "teleport arrival (one of five)", "misc/runekey": "key pickup (runic)",
    "misc/secret": "secret found", "misc/talk": "message beep (a trigger's message, chat)",
    "misc/trigger1": "a large switch (trigger sounds 3)", "misc/water1": "(by name) a small splash",
    "misc/water2": "(by name) a small splash",
    # Ogre
    "ogre/ogdrag": "Ogre's chainsaw dragging as it walks", "ogre/ogdth": "Ogre's death", "ogre/ogidle": "Ogre idle",
    "ogre/ogidle2": "Ogre idle (chainsaw)", "ogre/ogpain1": "Ogre's pain", "ogre/ogsawatk": "Ogre's chainsaw attack",
    "ogre/ogwake": "Ogre's sight",
    # plats
    "plats/medplat1": "platform (medieval), moving", "plats/medplat2": "platform (medieval), stopping",
    "plats/plat1": "platform (base), moving", "plats/plat2": "platform (base), stopping",
    "plats/train1": "moving train, moving", "plats/train2": "moving train, stopping",
    # player
    "player/axhit1": "an axe hitting the player", "player/axhit2": "the axe hitting a wall",
    "player/death1": "player's death", "player/death2": "player's death", "player/death3": "player's death",
    "player/death4": "player's death", "player/death5": "player's death", "player/drown1": "drowning",
    "player/drown2": "drowning", "player/gasp1": "gasping for air on surfacing", "player/gasp2": "gasping for air on surfacing",
    "player/gib": "gibbed", "player/h2odeath": "death underwater", "player/h2ojump": "(by name) jumping out of water",
    "player/inh2o": "entering water", "player/inlava": "entering lava", "player/land": "landing",
    "player/land2": "a hard landing (falling damage)", "player/lburn1": "burning in lava", "player/lburn2": "burning in lava",
    "player/pain1": "player's pain", "player/pain2": "player's pain", "player/pain3": "player's pain",
    "player/pain4": "player's pain", "player/pain5": "player's pain", "player/pain6": "player's pain",
    "player/plyrjmp8": "player's jump", "player/slimbrn2": "burning in slime", "player/teledth1": "telefragged",
    "player/tornoff2": "(by name) torn apart", "player/udeath": "gibbed death",
    # Shambler
    "shambler/melee1": "Shambler's claw swing", "shambler/melee2": "Shambler's claw swing",
    "shambler/sattck1": "Shambler's lightning, charging", "shambler/sboom": "Shambler's lightning bolt",
    "shambler/sdeath": "Shambler's death", "shambler/shurt2": "Shambler's pain", "shambler/sidle": "Shambler idle",
    "shambler/smack": "Shambler's claws connecting", "shambler/ssight": "Shambler's sight",
    # Grunt
    "soldier/death1": "Grunt's death", "soldier/idle": "Grunt idle", "soldier/pain1": "Grunt's pain",
    "soldier/pain2": "Grunt's pain", "soldier/sattck1": "Grunt's shotgun (its first 80 ms are weapons/guncock's)",
    "soldier/sight1": "Grunt's sight",
    # weapons
    "weapons/ax1": "axe swing", "weapons/bounce": "grenade bouncing", "weapons/grenade": "grenade launcher firing",
    "weapons/guncock": "shotgun firing", "weapons/lhit": "lightning gun beam (loops while it fires)",
    "weapons/lock4": "ammo pickup", "weapons/lstart": "lightning gun starting", "weapons/pkup": "weapon pickup",
    "weapons/r_exp3": "explosion (rockets, grenades, barrels)", "weapons/ric1": "nail ricochet",
    "weapons/ric2": "nail ricochet", "weapons/ric3": "nail ricochet", "weapons/rocket1i": "NAILGUN firing (sic)",
    "weapons/sgun1": "ROCKET LAUNCHER firing (sic)", "weapons/shotgn2": "super shotgun firing",
    "weapons/spike2": "super nailgun firing", "weapons/tink1": "a nail hitting a wall",
    # Scrag
    "wizard/hit": "a Scrag's spit hitting (the client's TE_WIZSPIKE)", "wizard/wattack": "Scrag's attack",
    "wizard/wdeath": "Scrag's death", "wizard/widle1": "Scrag idle", "wizard/widle2": "Scrag idle",
    "wizard/wpain": "Scrag's pain", "wizard/wsight": "Scrag's sight",
    # Zombie
    "zombie/idle_w2": "(by name) Zombie idle", "zombie/z_fall": "Zombie falling down", "zombie/z_gib": "Zombie gibbed",
    "zombie/z_hit": "a thrown gib hitting", "zombie/z_idle": "Zombie idle", "zombie/z_idle1": "Zombie idle",
    "zombie/z_miss": "a thrown gib missing, splatting", "zombie/z_pain": "Zombie's pain", "zombie/z_pain1": "Zombie's pain",
    "zombie/z_shot1": "Zombie throwing a gib",
}


def category(name: str) -> str:
    d = name.split("/", 1)[0]
    if d in MONSTER:
        return f"monster: {MONSTER[d]}"
    if name.startswith("misc/menu"):
        return "menu"
    return {"ambience": "ambience", "buttons": "world: buttons", "doors": "world: doors", "plats": "world: platforms",
            "items": "items", "player": "player", "weapons": "weapons", "misc": "misc"}.get(d, d)
