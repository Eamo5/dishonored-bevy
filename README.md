# Dishonored — Bevy rewrite

A re-implementation of Dishonored (2012) on the [Bevy](https://bevyengine.org) engine (0.19).
It reads the content of an installed copy of the original game (Unreal Engine 3 packages),
converts it once into GPU-ready cache files, and re-creates the game in Rust: the original
levels, materials (the D3D9 shaders translated to WGSL), lightmaps, animations, sounds and
music (Wwise), level scripts (Kismet and Matinee), interface art and fonts (Scaleform), and
the gameplay data of the original tweak objects.

No original game data is included in or produced into this repository; the cache is built
locally from your own installation.

## Requirements

- An installed copy of Dishonored (Steam). The game folder is found automatically on common
  Steam library paths; otherwise set `DISHONORED_DIR` to the folder containing `DishonoredGame`.
- Rust (stable) and a GPU with BC texture compression support (any desktop GPU).
- Optional: ffmpeg on the path (or `DH_FFMPEG`) to convert the Bink movies (the intro, the
  loading screens' loops, the title card, the credits); without it they are left out.

## Quick start

```sh
# 1. Convert everything up front (a few minutes the first time, ~3 GB in ./cache)
cargo run --release -p dhtool -- cook-all         # the missions (and the movies, through ffmpeg)
cargo run --release -p dhtool -- cook-audio       # Wwise banks
cargo run --release -p dhtool -- cook-ui          # fonts, interface art
cargo run --release -p dhtool -- cook-gamedata    # powers, charms, stores, texts, campaign scripts

# 2. Play
cargo run --release -p dishonored                      # main menu (New Game starts at Dunwall Tower)
cargo run --release -p dishonored -- --map L_Streets1_P
```

If a map hasn't been cooked yet, the game converts it on first load (with a progress screen).
Loading a cooked map takes about 0.1–0.6 s.

### Game options

| Option | Meaning |
| --- | --- |
| `--map NAME` | Map package to load (`L_Prison_P`, `L_Tower_P`, `L_Pub_Day_P`, ... — see `cache/maps`) |
| `--spawn N` | Use player start N (default: the mission's opening start, else the safest entrance) |
| `--recook` | Re-convert the map before loading |
| `--tex N` | Maximum texture size when cooking on demand (default 2048: the original PC textures' full size) |

A map started directly gives Corvo what he would have by then: Blink after the Outsider's
dream, the pistol, crossbow and Heart from the Hound Pits, some runes, and the story flags of
the missions before it.

## Controls

The original PC bindings (`DefaultInput.ini`); the keyboard ones can be changed in
Options > Controls (prompts and tutorials name the keys bound). Options also hold mouse
sensitivity and inversion, field of view, master / music / effects / voice volumes,
subtitles, brightness (scaling the level's display gamma in its colour LUT), windowed or
fullscreen display, vertical sync and the crosshair.

| Input | Action |
| --- | --- |
| WASD / Space / C / Shift | Move / jump and mantle / crouch (sneak) / sprint |
| Q / E (hold) | Lean |
| Left mouse | Sword attack |
| Ctrl (hold) | Block (timed = parry); behind an unaware enemy: choke (keep holding) |
| Ctrl + left mouse | Blood Thirst fatality (adrenaline full) |
| Right mouse | Use the left hand: power, pistol, crossbow, gadget, the Heart |
| 1–0, mouse wheel | Shortcuts (the original auto-assign order) |
| Middle mouse (hold) | Quick-access wheel (time slows) |
| F | Use (doors, pickups, levers, whale oil tanks, rewire, alarms); pick up / drop a body |
| Left mouse (falling onto someone / carrying a body) | Drop assassination / throw the body |
| Alt | Zoom (mask optics; F while zoomed: the second lens) |
| R / T | Health / mana elixir |
| J (Tab / Shift+Tab: pages, Left/Right: sub tabs, arrows: lists, Enter: act) | Journal: objectives, notes, powers (runes), bone charms, inventory |
| F or Enter (hold 1 s) | Skip a scene (the HUD's skip gauge fills round) |
| F5 / F9 | Quicksave / quickload |
| Esc | Pause menu |
| F3 / V | Debug overlay / noclip |
| F4 | The original's debug exec `KUWA`: the post-process graph's painterly Kuwahara branch |

## What's in

- **Powers** (levels, ranges, durations from the original tweaks): Blink, Dark Vision, Bend
  Time, Wind Blast, Possession (animals, and unaware people at level 2; inside a wolfhound,
  a swarm's rat or a fish Corvo has its body, `DisTweaks_Possessable`'s size and speeds: a rat
  keeps to the surface of water, a fish swims anywhere in its water without breathing; a river
  krust stays rooted and spits where he looks), Devouring Swarm
  (the original rats); enhancements Vitality, Blood Thirst, Shadow Kill, Agility. Runes buy
  them in the journal. Mana regenerates a portion above the last expense, like the original.
  Going into a host plays the original transition (`POSSESSION_IN_INST`, a
  `MaterialInstanceTimeVarying` whose parameter curves are cooked into `scene.post_curves`:
  the eye opening, the retina's veins, the host's view coming in), coming out
  `POSSESSION_OUT_INST`; inside, the view takes the possession node's warp and grading,
  warping harder as the end nears (`m_WhileMaxDistort`, `m_WarnMaxDistort`).
  The Mark of the Outsider glows on his hand as he casts or switches powers: the first-person
  clips' own particle notifies (`AnimNotify_PlayParticleEffect`, `Ps_Tattoo_Glow_*` at the
  arms' `Tattoo` socket, cooked into `scene.arm_fx`), drawn with the arms.
- **Dark Vision as the original draws it** (`Twk_DarkVision` and the post-process graph's Dark
  Vision node, `AltScreen_Effects.PostProcessChain.Test_PPG`): the eyelid closes (top and
  bottom, `PPG_EyeLidFinal_Mat`) and opens from the middle on the power's look
  (`PPG_DarkVisionFinal_Mat`: darkened corners), the world takes the tweak's
  `m_StaticUberAdjustement` (desaturated sepia) over the level's grading, and living beings
  are drawn as souls (`DarkVision_PMAT` / `DarkVision_Souls_INST`: yellow, brighter at the
  silhouette, a stirred behind colour through walls, fading out at the power's reach) into a
  mask mixed over the graded image in display space; at level 2 items (green) and security
  devices (blue) too. Characters' sight shows as the original cone particles from their eyes
  (`Ps_DVision_VCone_Blue_01`, red eyes in a fight) and heard noises as `Ps_DVision_Sounds`.
  The soul and composite shaders are the original pixel shaders (`dhtool mapshader`) by hand;
  the soul pass is a custom render phase on Bevy's mesh pipeline after the grading pass.
- **The post-process graph's screen warps** (`ppgraph.rs`): the original vector-field
  materials (`PPG_BendTimeVectors`, `PPG_BlinkVectors`, `PPG_UnderWaterVectorField`,
  `PPG_KOVectors`, the plague's `PPG_AdrenalineVectors`), their `TPpMaterialPixelShader`
  translated at load (`ue3prog::build_post`), drawn in turn into a quarter-resolution float
  field the way the graph's switches chain them, then the original motion blur
  (`FArkPpMotionBlur2PixelShader`, `ue3prog::build_global`) shifts the image by the field's
  offset and smears it along its vectors. Bend Time warms up into a radial zoom and noisy
  edges with its node's grading (post desaturation 0.95) over `Twk_BendTime`'s post effect
  warm-up and cool-down; Blink's aim bulges the lens with `Twk_Blink`'s warm-up wobble, the
  move zooms (`m_fMoveBlurMaxStrength`) and the arrival wobbles away
  (`m_CoolDownWobbleCount`); under water the image ripples. The scripts' screen effects
  (`DisSeqAct_PostProcess`): `Epp_Knocked` blurs along the view's turning and blends in the
  last frame (`KO_Combine` / `TEST_PPG_BackupRed`), `Epp_Weepers` takes the plague's warp and
  green, grainy grading. Each node's own grading (`m_bOverrideUberPp`, cooked into
  `gamedata.post_nodes`) layers over the level's like Dark Vision's. `DH_NO_PPG=1` turns the
  branch off.
- **Depth of field** (`dof.rs`): the level's (or the scripts') far blur, as the original's
  `FArkPpDofDownsample` + DOF uber pass do it: the scene mixed with a quarter-resolution copy
  by `saturate((depth - focus) / radius) x far blur`, before the grading, softening distant
  views. `DH_NO_DOF=1` turns it off.
- **The Heart**: beats faster near runes and bone charms; whispers the original secrets about
  the person targeted (by story group) or the place (by mission progress). Held, it marks the
  runes and bone charms still to be found with the HUD movie's `runeMarker` /
  `boneCharmMarker` as `m_RuneMarkerSettings` / `m_BoneCharmMarkerSettings` place them (90% at
  10 m to 150% at 5 m, fading as Corvo nears, the distance shown near the middle, kept at the
  screen's edge with the locator turned toward them when off it); with one within its reach and
  the Heart not in hand, the pawn's `m_HeartTargetTutorialMessage` ("Rune or Bone Charm
  nearby. Equip the Heart.") comes up once per approach.
- **The journal as `UI_Journal` draws it** (`jview.rs`, `journal.rs`), in the original's tab
  order (Objectives, Notes, Powers, Bone Charms, Inventory): the pause screens' drifting
  backdrop, the tab bar (`lib_mTabs_tab_s`, the chosen tab light and lifted), each page's own
  panel, sub tabs (`j_SubTabsBkgd`), lists and details panel laid out as the page classes do
  (`j_ObjectivesScreen`, `j_LogsScreen`, `j_PowersScreen`, `j_BoneCharmsScreen`,
  `j_InventoryScreen`). Objectives: the level scripts' objectives and tasks under their
  `PRIMARY` / `OPTIONAL TASKS` headers (done and failed ones ticked or crossed, the tracked
  ones' markers, the chosen one bracketed) beside the chapter's targets and briefing; the
  mission's clues beside its picture; its items (the abstract items whose
  `m_JournalDisplaySection` is the class's `DJIS_Mission`: Heretic's Brand, the rat viscera, Lady
  Boyle's invitation... as cards with their `m_JournalIconName` pictures; picking one up puts it
  in the pickup log rather than the note reader). Notes: written notes (and the location
  maps, drawn in the details), books, audiographs (`PLAY AUDIOGRAPH` / `STOP`). Powers: the
  six powers and four enhancements on their discs (lit when owned, the buy arrow when the
  runes allow, the `ACQUIRE` / `UPGRADE` tooltip that buys), with the mana it takes (`Very
  Low`... `None`), cost and runes in the details over its two levels — `LEVEL I` / `LEVEL II`
  with their badges, `(acquired)`, each level's words and the strategic tips (`RPG.int`'s
  `DisUISelectionType`), pale once acquired, the wheel scrolling them. Bone charms: the charms found, worn ones in their sockets, the slot bar
  (`ACTIVATED BONE CHARMS: n/m`). Inventory: the original sub-tabs — `RESOURCES` (coins,
  elixirs, runes), `KEY RING`, `GADGETS` (his blade, pistol, crossbow, the Heart; grenades,
  springrazors, rewire tools), `AMMO`, `UPGRADES` — as the 4 x 3 tilted cards with counts,
  the chosen one's picture, name and description beside them. Shapes cut out of bitmaps (the
  targets' brush frames) are drawn at cook time with the bitmap as their pattern. It moves as
  the movie does: the page fades up as the journal opens (0.3 s), slides 250 in from the side
  it was moved towards when its page or sub tab changes (`SetContent`: 0.25 s, Strong.easeOut,
  the sub tabs staying put), and fades as it grows to 105% when put away (`CloseJournal`).
- **The save and load screens as `UI_LoadGame` draws them** (`savescreen.rs`): the list turned
  -2.5 degrees, each save on its brushed card with its mission's strip (`UI_Mission_Small`),
  its date in the original's `m_DateFormat` (local time) over its chapter (`AUTOSAVE - ` /
  `QUICKSAVE - ` before it), the chosen card light; saving offers `CREATE A NEW SAVE` first;
  in the main menu the chosen save's mission picture with its date and name up the turned
  banners (`LoadGameDetails`).
- **The options as `UI_OptionsMenu` draws them** (`optscreen.rs`): every setting of the
  original's `EProfileSettingID` in its order, with `ArkProfileSettings`' defaults and ranges
  (`General`: Gameplay — difficulty, `Kill Cam Mode`, `Auto Use Mana Elixir` (a power short of
  mana drinks a remedy), `Auto-Save In Journal` (opening the journal saves to the autosave, not
  in a fight: `m_bDisableAutosaveInCombat`), `Head Bob Amount`, `Chains Climbing Relative to
  Camera` (looking down, forward climbs down) — and User Interface: `Show Health/Mana Gauges`
  Off / Contextual (a while after they change, and while low) / Always, objective popups,
  tutorial notifications (the hints and their window), interactions (the window by the
  crosshair), focus highlight, pickup log, contextual icons (the special moves'), player stance
  (the stealth shroud), objective, grenade, awareness and Heart markers, `Crosshair Style`
  Off / Simple (the dot alone) / Normal, `Crosshair Movement` (the reticles open with the
  weapon's dispersion), `Crosshair Opacity`; `Controls`: Keyboard Mapping, Mouse Settings
  (sensitivity, invert, `Smoothing`); `Graphics`: gamma, `Resolution` (the display's video
  modes: an exclusive full-screen mode, or the window's size), full screen, v-sync, field of
  view (65-110), `Texture Details` Low / Medium / High (a level loads its textures up to 512,
  1024 or their full 2048), `Model Details` Normal / High (the characters' lesser LODs at a
  distance, or never), `Light Shafts`, `Anti-Aliasing Mode` Off / MLAA (SMAA) / FXAA (over 4x
  multisampling), `Rat Shadows` (the swarms' rats cast shadows; off as the original's
  `bAllowRatsShadow`); `Audio`: the volumes, `Subtitles Mode` Off / Main Dialogue (no barks) /
  All Speeches, `Speaker Configuration` Auto / Stereo / 5.1 (see Sound below)) as tabs along the
  screen's -4 degree slant, their sub-categories on the
  dark band, the settings' rows with the original widgets — two choices side by side, a value
  between arrows, a slider's thumb with its number, a key binding's box — labelled with the
  original words (`Settings.int`). Tab / Shift+Tab and Q / E move between them, R restores the
  category's settings.
- **Weapon dispersion and recoil** (`aim.rs`): the pistol's and crossbow's shots stray within
  a cone that closes to the weapon's least dispersion while Corvo keeps steady and opens to its
  most as he sprints, leaves the ground or blocks (`DisTweaks_WeaponRanged_Attributes` by
  difficulty, `m_bMaxDispersionOn*`, at its interpolation speed; the accuracy upgrades'
  `Attribute_Dispersion*` off it). The dispersion is how far the reticle's brackets stand out
  (`Crosshair_Gun.SetDispersion` / `Crosshair_Crossbow.SetDispersion`: moved, turned and dimmed
  as it opens), so the shot lands within them; each shot kicks the view up
  (`m_fCamRecoilOnFire` less `Attribute_RecoilReduction`) and it settles. The reticles' brackets
  also take the crosshair state (empty, over an enemy) as `SetCrosshairState` sets them.
- **Head bob** (`arms.rs`): the original's `DisAnimNodeBlendByHeadBob` — as Corvo walks, runs,
  sprints or sneaks, `Ply_Head_Locomotion_as`' additive head clips for the pace
  (`ADD_Head_<Walk|Run|Sprint|Sneak><N|S|W|E>`, blended by the way he goes, in step with his
  stride; `ADD_Head_Mantle_Low` through a mantle, `ADD_Head_Rope_Climb` on a chain) move
  camera_jnt and the view goes with it, scaled by `Head Bob Amount`; his arms keep to his body.
- **Beheadings and severed limbs** (`gore.rs`): a finisher is a beheading now and then
  (`m_HeadChop`, `m_fBeheadRandomChance` 25%: `Sword_Ready_Fatality_Behead_A/B_Master` with the
  victim's `Generic_Fatality_Behead_*_Slave`); the victim's clip cuts at its
  `DishonoredNotify_SeverLimb` (`neck_jnt`): the body and the falling head (a physics piece
  pushed along the bone) each take the mesh's dismemberment LOD (the last, its sections the
  mesh's `m_MaterialsToBodyParts`: whose piece, which cut, a cap shown once cut), so both show
  their stump's cap; `SLInfo_Default`'s blood plays at `Gore_Neck` and `Gore_Head`, and lands on
  the lens nearby (`blood_heavy`). Cuts are saved.
- **Character LODs** (`npc.rs` `model_details`): the skinned meshes' lesser LODs are cooked
  (each `FStaticLODModel` read as the first) and shown with distance as UE3 picks them: LOD n
  once its `LODInfo.DisplayFactor` exceeds the screen radius / 320 (about 24 m for a guard at
  1080p; the swarms' rats too), on `Model Details` Normal; High keeps the best.
- **NPC finishers and stomps** (`npc.rs` `npc_finishers`): a sword-carrying NPC's killing blow
  on a foe of another faction (guards against weepers, thugs, assassins) is its sword's NPC
  fatality (`DisTweaks_NPCFatality`, `m_fChanceOfFatality` 1, `DisNPCAnim_NPCFatality`: the
  frontal impale), the killer playing `Generic_NpcVsNpc_FatalityStrike` and the victim, set by
  its clip's `anchor_jnt`, `Generic_NpcVsNpc_FatalityDeath`; among its other blows, now and then
  the stomp (`DisTweaks_NPCStomp`, `DisNPCAnim_NPCStomp`: `DisNPCAnimDefinitions`' pair
  `Generic_NpcVsNpc_FatalityKick` over the foe's `..._FatalityKicked`, a bash for the blow's
  damage).
- **Breath** (`breath.rs`): sprinting winds Corvo (`DisTweaks_PlayerPawn` breathlessness: Corvo's
  `m_fBreathlessnessSpeedSprint` 0.03 a second, `m_fBreathRegainSpeed` 0.08); his panting
  (`Snd_P_Sprint_Breath`) plays while it rises and his catching his breath
  (`Snd_P_Sprint_Breath_Stop`) while it falls; how winded he is sets the game parameter
  `m_BreathlessnessRTPC` (`Player_Sprint_Breath`), whose curves on both give their volume
  (silent until a fifth winded) and pitch (-250 to +50 cents), and his head heaves with
  `ADD_Head_OutOfbreath`.
- **RTPC curves** (`dhcook::audio` `RtpcCurve`, `audio.rs` `Rtpcs`): every sound object's
  `InitialRTPC` curves are cooked (the NodeBaseParams read through: effects, bus, the property
  bundles, positioning — 2D's panner, 3D's type, attenuation, spatialization, a path's vertices
  and playlist —, the advanced settings and states; 12,716 of 12,756 sounds and every container),
  those of a played object's ancestors with them, and evaluated as Wwise does (its curve
  shapes; a dB-scaled volume curve's values in its editor's amplitude space over -96.3..0). The
  game parameters the game sets drive them: `Player_Sprint_Breath`, and `TimeDilation` — on 225
  objects, the pitch an octave down as time halves —, set for each sound as `UDisAkComponent`
  does: the world's time scale for the world's sounds (Bend Time, slow motion), Corvo's for his
  own.
- **Sound output and Speaker Configuration** (`speakers.rs`): the sounds play on an output opened
  for `PSI_AudioPC_SpeakerConfiguration` — Auto (the device's own layout), Stereo, or 5.1 (six
  channels, FL FR FC LFE BL BR; the system downmixes it where it has fewer) — each through a
  panner that sets a world sound among the speakers by its direction from the listener (pairwise
  constant power between the speakers about it, as Wwise's 3D positioning: the front pair and
  the surrounds), spreading over them all within 1.5 m, and plays a screen sound on the front
  pair; changing it reopens the output and starts again what was sounding. Pausing the game
  pauses what was sounding (the original's `Pause_All` / `Resume_All`).
- **The painterly debug branch** (`kuwahara.rs`): the post-process graph's `KOKuwa` /
  `KuwaHalfRes` / `KuwaFullRes` nodes, which the original shows only on its debug exec `KUWA`
  (`KuwaSwitch` off by default): on F4, the scene at half size through the original's 5 x 5
  Kuwahara shader (`FKuwaPixelShader5`, strength 0.9), back to full size through the 3 x 3
  (`FKuwaPixelShader3`, strength 2), as disassembled.
- **Light shafts** (`lightshafts.rs`): the maps' shaft-casting light (`bRenderLightShafts`: the
  sun of 26 maps, a point light on Kingsparrow) draws UE3's light shafts — the original's
  downsample, radial blur and apply shaders ported as disassembled: the occlusion (near things
  against the far within `OcclusionDepthRange`) darkens towards `OcclusionMaskDarkness`, the
  bloom near the light (over `BloomThreshold`, `BloomScale`) is drawn out towards it twice and
  added tinted (`BloomTint`) where the scene is dark (`BloomScreenBlendThreshold`); fading as the
  light comes round behind. `Light Shafts` turns them off.
- **The store as `UI_Shop` draws it** (`store.rs`): `PURCHASES` and `UPGRADES` tabs (the
  upgrades under `Weapon Upgrades` / `Equipment Upgrades`), the tilted cards four across with
  their price tags (red and crossed out when out of reach, padlocked while a prerequisite is
  missing), the chosen item's picture, `Owned:` count, cost and coins carried.
- **The mission statistics as `UI_MissionStats` draws them** (`mission.rs`): the mission's
  illustration under Corvo and the shards, its name on the brush with its dark shadows, the
  statistics list (values, ticked boxes) turned with its section bands, and the things found
  (`FOUND`: runes, bone charms, shrines, paintings, coins with their icons, each out of how many
  the mission holds: its tweak's `m_MissionStatsMaxValues`).
- **Notes read as `UI_Note` shows them** (`notescreen.rs`): a note or book picked up opens over
  the blurred game on the dimmed, drifting backdrop — its title dark on the brush, its words in
  the scrolling field — and a location map in its frame; Escape puts it away (it stays in the
  journal).
- **The menus' questions and the saving icon as `UI_Global` draws them** (`msgbox.rs`,
  `globalui.rs`): overwriting a save, loading one, leaving for the main menu or Windows,
  starting a new game over autosaves and restoring a category's settings ask first with the
  original words (`t_Q_SaveGame`, `t_Q_LoadGame`, `t_Q_BackToMainMenu`...) on the veiled
  message box with its YES / NO buttons; each save shows the turning gears
  (`g_SavingNotification`) in the corner.
- **Menus over the game blur and grey it** as the original's do (`m_bBlurGameWhileActive`:
  the journal, pause, store, mission statistics): `DisGlobalUIManager`'s `UI_Blur` (far blur
  0.9 from the eye, half desaturated) fades in and out over 0.2 s; the HUD and subtitles hide
  under them.
- **Audiographs**: the 55 punched cards (`DisAbstractItemAudioLog`) and the players that hold
  them (`DisAudioLogPlayer`, ~50 across the missions and the Hound Pits variants). "Use" pushes
  the card in (its `In`/`Loop`/`Out` sequences) and plays its recording: the speaker's
  `Dlg_AudioGraphs` blurbs (each `DisConv_Hook_PlayAudioLog` link's `m_iBlurbGUID` → Wwise
  event), one after another, with their words as subtitles and the player's hiss
  (`Snd_UI_Audiolog_Noise`) before and after; "Stop the audiograph" cuts it. Heard once, the
  card is filed in the journal's Logs under `AUDIOGRAPHS`, with its description and
  `PLAY AUDIOGRAPH` / `STOP AUDIOGRAPH` to replay it from there.
- **Bone charms** (the 29 originals, found at random, worn up to the slot count), **stores**
  (Piero's workshop, Griff) with the original items, prices and blueprint requirements.
- **AI**: factions from the original data (guards, thugs, assassins, weepers, wolfhounds,
  tallboys on stilts with bows), sight, hearing, searching, ranged weapons, alarm bells
  that call reinforcements; feuding factions fight each other; weepers lunge and seize
  Corvo's arm (`DisTweaks_WeeperGrab`: break free with five attacks, or step out of reach).
  Spawners put their characters in other factions where the original does
  (`m_pFactionTweakOverride`: the Tower's guards serve Corvo, Lady Boyle's guests and guards
  are neutral, the Golden Cat and the Lighthouse have their own); armed characters Corvo
  hurts turn on him.
- **Combat by the original numbers**, in its units (Corvo has 70 health): each character's
  health, regeneration, accuracy and parrying are its pawn's attribute tweak
  (`m_pAttributeTweaks[1]`, the only slot authored, its `DisAttribute`s by difficulty: a
  prison guard 20, a city watch guard or overseer 40, an elite 50), its weapons those of
  its `m_ContentInventoryLoadout` (a sword's `m_MeleeDamage` 10/15/20/30 by difficulty, an
  elite's pistol `m_RangedDamage` 15/20/25/35, a tallboy's bow, an assassin's wrist bow).
  Corvo's sword deals `Twk_Inv_SwordCorvo`'s 10; guards parry with `m_ParryChanceOfStarting`,
  then `..OfChaining` up to `m_ParryChainsAllowed` in a row, a parry breaking their guard at
  `m_BlockBreakRate_Min`..`Max`; a character off balance from Corvo's parry dies to the
  counter (a fatality). His pistol's bullet (`Twk_Proj_BulletUpgraded`) deals 20, ×1.5
  within 9 m, and kills with a head shot (`m_bKillOnHeadshot`); a crossbow bolt deals the
  crossbow's 20, doubled on someone unaware (`m_fDamageMultiplier_Stealth`). Characters'
  shots hit at their `m_MaxAccuracy` close, falling to `m_MinAccuracy` at the end of their
  reach; the hurt regenerate `m_HealthRegenAmount` every `m_HealthRegenRate` s up to
  `m_HealthRegenLimit` once `m_HealthRegenInitialDelay` has passed. Overseers throw grenades
  as their grenade weapon's contexts do: lobbed at Corvo 10-40 m off
  (`DisTweaks_NPCLobGrenadeAtUnreachable`, 25 m/s, every 7-12 s, 3-5 s while he's out of
  reach), tossed leaping back from him 2.5-8 m off (`DisTweaks_NPC_OverseerJumpAway`), each
  grenade with its own fuse and blast (`Twk_Proj_Grenade_Overseer_*`: 3.5 m to characters,
  6 m to Corvo); the HUD movie's `grenadeIndicator` warns of each live grenade in reach,
  its arc turned toward it and its glow pulsing faster as the fuse burns. The unconscious
  are named `(Unconscious)` under the crosshair. Enemies in an ambient scene (a matinee's
  loop: an overseer questioning a guard) break off it to fight Corvo. Swordsmen fight with
  their blade's moves (`DisTweaks_NPCAttackShort` within 2.35 m, `..Medium` lunging from
  2.35-3.75 m, `..Long` from 3.75-5.7 m, each within its angle and on its own cooldown, big
  blows at `m_fRandomBigHitChance`, with the matching `Sword_Attack_*` clips) and answer
  Corvo's block with a kick (`DisTweaks_NPCBash`: it breaks the block and shoves him back);
  a guard who parries plays its parry (`Sword_Ready_ParryWin_*`) and strikes back straight
  after (`DisTweaks_NPCRiposte`), one whose blow Corvo parries reels (`..ParryLost_*`), and
  one who isn't his enemy shoves him out of its way when he crowds it (`DisTweaks_NPCPush`,
  `Sword_PushBack_*`, with its "personal space" bark);
  his blows coming, they may step aside or back out of reach (`DisTweaks_NPCSideStep` 2.5 m,
  `..BackStep` 3 m, every 12-15 s at most; how readily is read from `m_DodgeLevel`, a tenth
  a level). A big blow leaves Corvo reeling for `m_fVulnerableTime_BigHit_Dizzy`; his perfect
  parry is a block begun within `m_fTapForPerfectParryWindow` (0.2 s), within his sword's
  `m_fBlockAngle` (50 degrees). When his blow and a guard's land together (their attack
  zones within `m_fVersusZoneMaxTime`, each facing the other) the blades lock
  (`StatePlayerMasterVersus`): "Mash [LMB] !" (`DUI_Context_Versus`), the guard straining in
  `Sword_Versus_MinigameUU` and his arms in `Sword_Ready_Versus_MinigameUU`. His presses over
  1.6 s against his `MeleeVersusMiniGame_WinBig` (10) / `_WinMedium` (5) / `_LoseMedium` (2)
  settle it: a big win leaves it reeling (`Sword_Versus_BigLose`, the next blow a fatality),
  a win staggers it, a stand-off parts the blades, and fewer throws him back with the guard
  pressing on (how the three thresholds combine is our reading; the data names them only).
  Script `clash` sets one up with the nearest guard. Having lost sight of Corvo within 7 m,
  a swordsman may lie in wait for him where it stands (`DisTweaks_NPCAmbush`; thugs crouch
  in `Sword_Ambush_In`/`_Loop`), springing at him with a lunging blow (`_Out`) as he comes
  within the context's 2.5 m, or giving it up after 8 s (`_Cancel`). Script `ambush [NAME]`.
- **Stealth as the original measures it** (`stealth.rs`): each character sees with its pawn's
  `DisTweaks_Vision` (a focused cone, 45 degrees out to 35 m, and a peripheral one, 160 degrees
  out to 17 m, each looking far less up than down: rooftops hide) and pays attention with its
  brain's `DisTweaks_PawnAttention` (one for an unsuspecting enemy, one for a suspecting one, one
  for everybody else). Seeing Corvo raises attention by his visibility
  (`Twk_PlayerVisibility`: his light value normalised over the darkest and brightest of the
  scripts' `DisSeqAct_SetPlayerVisSettings`, the `DisStealthVolume` he stands in or the map's
  `m_pMapPlayerVisSettings`, plus his speed; swimming under water hides) plus what nearness
  adds (`m_DistanceValueTuning`), scaled by difficulty; out of sight it leaks away. Its level
  follows the tweak's thresholds: the head turns to him, the body turns to face him, the
  character investigates, then he's busted. What it hears raises attention by the tweak's
  increases (an anomaly, danger). Sneaking, the HUD effects movie's black shroud darkens the screen's edges
  (`BlackShroud`, tweened in 0.35 s) until someone busts him, with the entering-sneak sound.
  `DH_ATTN_LOG=1` logs attention levels, `DH_VIS_LOG=1` the light range and sneaking.
- **The interface movies' animations as authored** (`flash.rs`): `dhtool cook-ui` also
  writes each Scaleform movie's timelines (`ui/<movie>/timeline.json`: every sprite's frames
  as the SWF has them — display-list places, moves, colour transforms and removals, frame
  labels, and the frame scripts' `stop`/`play`/`gotoAndPlay`/`_visible`), and the game plays
  them at the movie's frame rate with nested clips running on their own, driven from code the
  way the original's ActionScript drives them (`gotoAndPlay("fillIn")` on `right_mc.mc1`).
  Bitmaps are drawn with their Flash colour transforms (multiply, then add, in the movie's
  gamma space; `flash.wgsl`), clipped to their shapes (repeating fills tile) and cut by the
  masks over them (rectangular, where square to them: bars and reveals).
- **Awareness markers** (`awareness.rs`, the HUD movie's `awarenessIndicator` and the HUD
  tweak's `m_AwarenessMarkerSettings`): two mirrored fans of three lightning bolts around the
  head of whoever notices Corvo, played from the movie's own timeline: a bolt lights for each
  attention level reached (head track, turn to face, investigate: a red flash settling
  white), goes dark again as the attention falls, and all three fly off red when he is
  busted; the fans fade in as the marker comes up, out once the attention has leaked away,
  at once when the character goes down. Markers stand at 75% and vanish within 2 m
  (`m_ScaleVariation`, `m_AlphaVariation`), show on enemies and on those the scripts name
  (`DisSeqAct_OverrideAwarenessDisplay`: Emily seeking at hide-and-seek), and reaching turn to
  face plays the detection stinger (`m_pAwarenessStingerEvent` `Music_Player_Detected` at
  `m_AttentionLevelToPlayAwarenessStinger`). The script's `aware 1,2,3,4,0` steps every
  marker through levels (tests).
- **The location banner** (`location.rs`, the HUD movie's `hud_location` and its `Location`
  class): `DisSeqAct_ShowLocationDiscovery` puts the place's name up as the original does — a
  big first and last letter around the rest (Emerge BF at 42 and 31, stretched 1.2 high),
  revealed from the middle by the growing mask while the separators slide in across each
  other, the feathered ornaments open at either end and the backing widens from 30%; after
  the HUD tweak's `m_fLocationDiscoveryDuration` (5 s) it fades as the separators draw back,
  with the `Discovery` music cue. Script: `location NAME`.
- **The front end and pause screen as their Scaleform movies draw them** (`frontend.rs`):
  the title (`m_StartScreen`: the logo, "PRESS ANY KEY"); the main menu over the menu map's
  flying camera (`m_MainMenu`: blades, compass, splatters, logo, Corvo's portrait, and the
  slanted button bar at the bottom right laid out as `MainMenuButtonBar` lays it out —
  `m_MainMenu_btn_`s sized to their labels, the one chosen inverted on its light brush and
  grown); the new game's difficulties (`m_nGame`: the `lib_titleMc` title, the brushed list,
  Corvo's portrait and the description for the one chosen); the pause screen (`p_bkgd` and
  `p_pauseMenu`: Corvo's mask, the brush frame, "PAUSE" up the left, the list masked to its
  slanted panel; its backdrop drifts as `_common.AnimatedBackground` makes it: the grain at
  15%, the shards and blades flying in and swaying). Dead (once the death has faded,
  `m_fDeathFadeDelay`) or ended by the scripts, the same screen is the game over menu:
  "GAME OVER" up the left, the reason on its brush ("You have met your demise." or the
  scripts'), the mask sliced (`anim_slice`), and `Resume From Last Save`, `Load game`,
  `Back to Main Menu`, `Back to Windows`. The movies' vector shapes are drawn into images at cook time (`swfvec.rs`:
  solid and gradient fills, strokes) and the symbols they import from the shared library and
  the options and load screens are copied in (`ImportAssets`); the menu clips play in real
  time over the paused game. Script `flash MOVIE SYMBOL [X Y]` shows any symbol.
- **The Quick-access Wheel as `UI_PowerWheel` draws it** (`wheel.rs`): everything Corvo
  carries round the wheel as `WheelHandler` places it — the weapons in `weaponsOrder`
  centred at the top, the powers in `powersOrder` on from them, `360 / count` degrees each,
  their discs at 75% on the 500-wide circle's rim with their counts, the separators between
  the groups, the indicator turned to the one pointed at (dark on its light disc), its
  illustration (`item_schema`) and name in the middle.
- **The crosshair** (`crosshair.rs`, the HUD movie's crosshair symbols): the original dot,
  and over something to use the HUD tweak's `m_InteractionCrosshairSettings` symbol by
  `eCrossHairStatus` (a hand to take, a note with an eye to read, a hand to carry, a padlock
  for a locked door that can be broken down (its tweak's `m_pBreakSteps` authored, 187 of the
  383 doors) and the barred padlock for one that can't, a door with an arrow for a way out to
  another map (whatever its use goes on to set or go to the player's travel destination,
  `CHS_OVER_DOOR_LEVEL_TRANSITION`: 47 exits on 14 maps), a ring for anything else) keeping
  the dot where the tweak does; otherwise
  the left hand's reticle (pistol, crossbow, grenade, sticky grenade, spring razor, power) in
  its `EDisCrosshairState`: ready, empty (no ammunition, too little mana) or red over an enemy.
- **The interaction window** (`intwindow.rs`, the HUD movie's `hud_intWindow`): right of the
  crosshair, the name of what Corvo looks at over a faint rule and what he can do with it,
  in the HUD tweak's `m_InteractionTexts` (`[F] Loot` to take, `Hold [F] Carry` a body or a
  prop), on the window's brushed backing.
- **Move and context prompts** (`prompts.rs`): the HUD movie's `InteractionsIcons` at the
  bottom right — for each special move within reach, its `hud_interactionIc` icon and keys
  (assassinate `[LMB]` and choke `[Ctrl] [Hold]` behind someone unaware, pickpocket `[F]`,
  a drop assassination falling onto someone, a Blood Thirst fatality `[Ctrl] + [LMB]`, a
  ledge to mantle `[Space]`), stacked from the bottom and faded in 0.35 s; and its
  `InteractionsMessage` at the left — what Corvo holds or is in the middle of (`[F] Drop` /
  `[LMB] Throw` a body or a prop, `[F] Drop` / `[Space] Jump off` a chain, `[F] Exit
  Keyhole`, `Mash [LMB] !` in a weeper's grip, `[Alt] Exit Spyglass`, `[RMB] End Bend
  Time`, `End Dark Vision`, `End Possess`) sliding in under its rule. The words are the HUD
  tweak's `m_InteractionTexts` by `EDisUIInteraction`, cooked from `Twk_InGameUI.int`.
- **The tutorial window** (`tutwindow.rs`, the HUD movie's `TutorialWindow` at the bottom
  left): the first new rune, bone charm and mission clue bring up the HUD tweak's hint
  (`m_PressKeyToBuyPowers`, `..ToEquipBoneCharms`, `..ToReadChapterNoteMessage`) with its
  picture (`_img_mc`: a rune, a charm, a note), popping in from the left at 250%, its parts
  drifting to and fro while it stays its `m_fTutorialWindowDuration` (10 s), with the
  window's notification sound.
- **Cooking grenades**: the button held pulls the pin and the fuse burns in the hand — the
  HUD movie's `GrenadeCooking` gauge at the crosshair counts it in hundredths, pulsing every
  half second; let go, the grenade flies with what's left; `[F] Cancel Cooking` puts it back;
  held to the end, it goes off in the hand. Over someone asleep, the crosshair's info line
  (`crosshairInfosTxt_mc`) gives their type's `m_AsleepCrosshairText`.
- **The objectives popup** (`objnotify.rs`, the HUD movie's `hud_objW` and its
  `ObjectivesNotification` class): an objective added, updated, completed or failed puts up
  the original popup at the top right — the HUD tweak's title (`New Objective`, `Objective
  Updated`, `Objective Completed`, `Objective Failed`: dark capitals on the pale bar), the
  objective's name on the dark bar, the lozenge and its state's mark (`!`, a check, a red X
  with shards), then its tasks one at a time beneath (each fading in with a glow of its
  state's colour and its own mark, 3.5 s each), everything sliding and spinning in and out as
  the class tweens it, with the UI theme's objective sounds; popups queue. Script:
  `objpopup added|updated|completed|failed NAME|TASK|TASK:completed`.
- **Breath under water** (`oxygen.rs`, the HUD movie's `hud_oMeter` and its `OxygenGauge`
  class): the crystal gauge swings in at the bottom right over its swirling backing, its fill
  masked to the breath left with the pointer at its top (tweened 0.45 s), a glow pulsing
  behind; it fades once he has his breath back. Script: `breath N`.
- **Subtitles, game messages and tutorials** (`hudtext.rs`): placed, sized and coloured as
  the HUD movie's fields are, scaled with its stage — the subtitles (the body font at 24,
  `#e8f7db`, centred in a box from 582 units down), the game messages above them (from 522
  down, `#e3f2d6`) and the level scripts' tutorial above those (`TutorialMessage`: one at a
  time, fading in 0.35 s as it settles from 110%, out in 0.15 s); their dark drop-shadow
  filter is approximated. Script: `message TEXT`, `tutorial TEXT`.
- **Stance** (`playerstate.rs`, the HUD movie's `hud_icPlayerStates`, by
  `EDisUIPlayerState`): crouched, a figure crouching (or creeping, moving) plays in from
  standing at the bottom left over its swirl backing, and back up as he stands.
- **Taking damage as the original shows it** (`hudfx.rs`, the HUD effects movie
  `UI_HUDFX.HUDFX` and its `DirectionalDamages` class, disassembled): the screen's edges
  redden (the movie's edge strip on all four sides, the sides at a third, alpha raised by
  `0.01·d² + 0.4·d − 1` per hit, held `max(40·d ms, 2.5 s)`, faded over 3.5 s); the hit
  sprite (`dirHitSprite`, played from its timeline) lands on the screen's edge towards the
  blow, in from further out and larger: the lighter splatter under 20 damage, the heavier
  above with the cracked glass coming up over it, the glass shaking and blood flying off
  (more, and the glass glinting red, above 50, 70 and 90); held `max(35·d ms, 1.5 s)`, faded
  over 1.2 s; the arc (`_indic_mc`) points the way, flashing in and fading. Low on health,
  Corvo's `m_HealthEffects` put up their looping camera lens effects (`ps_Cam_NearDeath_01`
  under a third of his health, `_02` under a sixth: a red glow at the screen's edges) with
  the sounds
  his `m_BarkCues` give them (`Snd_P_Near_Death_Lvl_1/2`, the stinger, the regeneration as he
  recovers); his eyes coming out of water, `m_pWaterExitEffectTweaks` (`ps_camera_water`) runs
  down the lens. `DH_NO_HUDFX=1` puts back a plain red flash.
- **Health, mana and the left hand** (`gauges.rs`, the HUD movie's `masterHUD_mc` and its
  `PlayerGauges` class): the top left as the original draws it — the health and mana shards
  (tilted 15 and 30 degrees) over their ink splashes, each filling from its base to the
  amount (tweened 0.45 s) with a pointer at the level and a glow that pulses (15-40% over
  2.25 s, 35-75% over 0.55 s once under the HUD tweak's `m_LowHealthThreshold` 30% /
  `m_LowManaThreshold` 19%), and the left hand's power or weapon (`itemIcons`) beside them
  with its ammunition (a small UI material stands in for the shards' `mask_mc`).
- **The pickup log** (`pickuplog.rs`, the HUD movie's `PickupLog` class): what Corvo picks
  up comes in at the right of the screen (its name and small item icon, `UI_ItemIcons_Small`,
  over a brush stroke; sliding in over 0.4 s), the older entries rise a slot and dim (60%,
  20%); each stays 2 s and fades over 0.4 s. (The running tally of coins and runes the
  rewrite used to show is off; `DH_TALLY=1` brings it back.)
- **Objectives** come up at the top right as they change (`New Objective`, `COMPLETED:`, the
  HUD tweak's titles), stay 6 s and fade, as the original's objectives window opens and closes;
  the journal keeps them (`DH_OBJECTIVES=1` keeps the list up). Game messages show at the
  bottom of the screen like the movie's `gameMsg_mc`, for the HUD tweak's
  `m_fGameMessageDuration` (5 s); the scripts' tutorials stay `m_fTutorialDuration` (10 s).
- **Target cards** (`targetcard.rs`, the HUD movie's `TargetNotification`): when the scripts
  report a target's fate (`DisSeqAct_ShowTargetNotification`: assassinated, neutralized,
  rescued, spared), the card comes up at the top right with the target's portrait
  (`UI_portraits_objectives`) in its red stroked frame, the name from the chapter target
  tweak (`m_TargetName`) and the state in its colour, with its sound (`UI_H_target*`), for
  `m_fTargetNotificationDuration` (5 s). Script: `card KIND NAME PORTRAIT`.
- **The HUD's gauges** (`skip.rs`, the HUD movie's `hud_skGauge_` and `hud_chGauge_`): a
  scene is skipped by holding Use or Enter for `m_fSkipMatineeButtonTime` (1 s) while the
  skip gauge's blue ring fills round at the bottom left; choking someone, the interaction
  gauge's red ring fills at the screen's middle (drawn with a small radial-wipe UI material
  standing in for the movie's `_mask_mc` wipe).
- **Security**: walls of light, arc pylons and the whale oil tanks that power them; rewire
  tools turn them on their owners. Watch towers (`watchtower.rs`, `DisTweaks_WatchTower`)
  sweep the ground with their searchlight (the original `Regent_Light_Cone` beam from the
  head's `BeamOrigin_Socket`, the head and its gun and generator turning on the pole), alert
  on Corvo caught in it (chirp, the scripts' `DisSeqEvent_WatchTower` Activated /
  Deactivated), warn, and fire volleys of explosive arrows (`WatchTowerExplosiveArrow_twk`)
  at the tweak's cadence until he's out of sight for `m_fStateAttackTimeout`; without their
  tank they go dark, and looking into the beam up close blinds (the post-process graph's
  Blinded node). Rewired, a tower turns its beam and arrows on its owners; the scripts can
  turn towers and pylons off, on or round (`DisSeqAct_WallofLightControl`,
  `DisSeqAct_DefenceTower`) and order a volley at something
  (`DisSeqAct_WatchTowerShootAtTarget`). `DH_TOWER_LOG=1` traces them.
- **Gadgets**: grenades and sticky grenades, springrazors, incendiary bolts and explosive
  bullets with the original blast radii, arming times and effects; level pickups give the
  ammunition type their tweak names. Valuables carry their tweak's name and worth ("Take
  Tyvian Ore", `m_Quantity` coins).
- **Lighting**: the original light maps, the dominant lights through their signed
  distance-field shadow maps, the levels' runtime point and spot lights drawn with each
  material's own light-pass shaders (`DH_NO_DYN_LIGHTS=1` turns these off), each level's
  reflection cube (`WorldInfo.mSceneReflection`) on the surfaces that reflect it, and the
  characters' shadows from the dominant directional light (UE3's light attenuation buffer,
  from a shadow map of the dynamic objects). Gameplay lights (explosions, burning bolts) light
  the world through the same light-pass shaders, and characters take the passes of the runtime
  and gameplay lights near them (as UE3's light environments add the dynamic lights);
  `DH_NO_PART_LIGHTS=1` turns the characters' off. Characters' ambient light comes from the
  Lightmass volume samples, bucketed on 4/32/256 m grids by their reach: some levels have
  samples hundreds of metres wide, and one grid took the Flooded District 20 s to load.
  `DH_SPAWN_PROFILE=1` times each phase of a level's spawn.
- **Fog**: every `DisFog` layer of the map, drawn as the original's full-screen pass
  (`FDisFogPixelShader`): from the scene depth, the stretch of each view ray inside a layer's
  height band ramps, linearly or through the layer's LUT, to its opacity between its near and
  far planes, sparing the sky beyond its no-fog plane; sun layers glow towards the sun. Up to
  four layers at once, as in the original (interior layers join when there's a roof
  overhead); level scripts switch layers and streamed-out levels take theirs. Under water,
  the water's own fog hangs from its surface. `DH_NO_FOG=1` turns it off.
- **Swimming**: the levels' water volumes (`DishonoredWaterVolume`): Corvo swims at the
  surface and dives along his view (jump rises, crouch sinks, sprint strokes faster;
  `StatePlayerMasterSwim`, `WaterSpeed`), the arms play the original swim strokes, currents
  carry him, he holds his breath for 30 s (`m_fApneaMaxDuration`, a gauge under the mana
  bar) and drowns after (the drowning step is read as a tenth of his health); going in
  splashes by speed with the volume's sounds and effects, and under water its colour grade,
  fog and sound loop take over. Climbing out is a mantle.
- **Carrying bodies**: the unconscious and the dead ([F] Pick up), carried over the shoulder
  with the original carry clips (`Ply_Empty_CarryCorpse_as`; the body plays the matching
  `Corpses_CarryCorpse_*_Slave` clips in step, drawn with the view model), slower
  (`GroundSpeedCarryingCorpse`) and without sprinting or the sword; [F] puts the body down
  (the original drop clips) and attacking throws it. The level scripts hear of it
  (`DisSeqEvent_Corpse`: picked up, dropped, discovered by a guard), and an unconscious body
  that ends up in water drowns (a kill).
- **Assassinations**: attacking someone unaware of Corvo (from behind, or anyone who hasn't
  noticed him) plays the original paired clips by the side he takes them from — front, back,
  left or right; slow and deliberate while sneaking, quick otherwise, and their own variants
  with a body on his shoulder (`Sword_Ready_Assassination_<Fast?><Side><_CarryCorpse?>_Master`
  with the victim's `Generic_Assassination_..._Slave`). The victim is turned and set where its
  clip's `anchor_jnt` puts Corvo (a metre or so behind, before or beside it) and Corvo holds
  still for his clip. The targets and nobles die their own dramatic deaths instead
  (`Sword_DramaticDeath_Front_Campbell_Master` / `DramaticDeath_Front_Campbell_Slave`, Havelock,
  Martin, the Pendletons, the Lord Regent, Daud; `..._Back_A` from behind).
- **Finishers** (`DisTweaks_Fatality`): a sword blow that kills, the counter after a parry and
  the Blood Thirst strike play the original paired finishers (`Sword_Ready_Fatality_<Front |
  FastFront | SmallFront_<Side>>_*_Master` with `Generic_Fatality_..._Slave`, a target's
  dramatic death for a target; a beheading one time in four: see Beheadings), set up as the
  assassinations are. Their slow motion is the clips' own (`DisNotify_BendTime_Ranged` stretches:
  Corvo's and the world's time scales, eased in and out): the dramatic deaths' always, the
  finishers' (`..AdrenalineBendTime..`) for a Blood Thirst kill or as the `Kill Cam Mode`
  option has it — Off, Normal (the last foe fighting him, else one in three) or Frequent.
  Blood: the contact system's sword against a body (`Pfx_BloodMotion` spurting along the blow,
  held to the body; within 5 m one time in three `blood_light` on the lens) on every blade hit,
  and in the finishers and assassinations at their clips' own moments
  (`DishonoredNotify_PlayerMeleeWeaponHitFlesh`), with their blood on the lens
  (`AnimNotify_CameraEffect`).
- **The crossbow's kill cam** (`killcam.rs`, `DisCamera_FollowProjectile` with the crossbow's
  `m_KillCamSettings`): a bolt that will kill (`Twk_Proj_Arrow`'s `m_bKillCamEnabled`; the
  shot's damage against what the victim has left), when the `Kill Cam Mode` option takes it,
  is followed from just behind (`m_Offset`, pitched down a little) with a 30 degree view as the
  world slows to a tenth; after 14 m the camera holds and watches it strike, the world at 0.6
  for a second and a half, and then the view is Corvo's again. The HUD and his hands are put
  away meanwhile.
- **Drop assassinations**: attacking while falling onto someone kills them from above, with
  the original paired clips by side (`Sword_Ready_Assassination_Drop*_Master` /
  `Generic_Assassination_Drop*_Slave`); the body breaks the fall, and the Falling Star charm
  gives mana (`DropAssassinationManaBonus`).
- **Chains**: the levels' hanging chains (`DisClimbable`) can be climbed: walk or jump into
  one, forward and back climb it, jump lets go (or mantles onto a ledge), crouch drops; the
  chain rattles as Corvo climbs.
- **Objective markers** (Options > Objective Markers, `markers.rs`): the HUD movie's
  `objectiveMarker_primary` (a downward chevron) over the target the level scripts give each
  visible active task (`DisSeqAct_UpdateTaskTarget`; a character is followed), as the HUD
  tweak's `m_TaskMarkerSettings` place it — 10 units above, 90% far off growing to 150% near,
  fading out within 10 m — with its distance on the description's brushed backing while it
  is near the middle of the screen (`m_fMarkerFocusDistance`); off screen it is not shown
  (`m_bKeepBBInMarkerArea`). `DH_MARKER_LOG=1` logs where they point.
- **Mission openings**: the level scripts' autosaves (`DisSeqAct_AutoSave`) go through (to
  the Autosave slot), so each mission's opening runs as in the original: Samuel's boat
  carries Corvo in (the player's matinee group rides the boat's bone; he steps off at the
  scene's drop area) while Samuel talks, and the objectives arrive after.
- **Choices**: the level scripts' choices (`DisSeqAct_DialogScriptedChoice`: going on to the
  next area or staying, "[I'll go to sleep now.]", "[Infect Bootleg Elixir]") are put to the
  player as the HUD movie's `PlayerChoice` menu draws them (`hud_playerChoice`: the options
  stacked up from the bottom left of the middle, each a `hud_plC_button` with its circle, the
  arc beside them; the highlighted one plays `over`), the screen letterboxed meanwhile (the
  choice state's `m_bUseLetterboxing`: `hud_blackStripes`): the number keys, or Up / Down and
  [Use], answer; walking away leaves them unanswered. Script: `choice A|B|C` (with
  `DH_CHOICE=show` to leave it up).
- **Event messages**: the HUD tweak's game messages for runes and bone charms found
  ("2/3 Runes found", the first time "New Rune added / Press [J] to acquire Powers", "New
  Bone Charm added / Press [J] to equip") and mission clues ("Mission Clues updated / Press
  [J] to read").
- **Pickpocketing**: what a character carries (its spawner's `m_pStealablePickup`: a coin
  purse, a key) hangs at its belt; Corvo can take it from behind someone who hasn't noticed
  him, or from the body.
- **River krusts**: the Flooded District's shellfish (`DisRiverKrust`) play their tweak's
  states (`m_AnimSequences`): shut in the shell, opening when someone comes within range and
  in sight, spitting volleys of acid that arc and lead a moving target (damage by
  difficulty, the trail and splash effects), clamping shut when Corvo comes close or strikes
  the shell. Damage goes through the original's filters: blades and bolts glance off a shut
  shell, bullets barely scratch it, explosions kill it; open, one hit does. A dead krust's
  pearl can be taken (25 or 50 coins). The level scripts' volleys at their targets
  (`DisSeqAct_RiverKrustSpitAtTarget`) play too.
- **Hagfish**: the fish (`DisFish`) roam the water around their place; a swimmer near them is
  set upon and bitten (`m_DamagePerHit` by difficulty, at most every `m_fMinTimeBetweenBites`),
  bodies in the water are eaten, and a blade, bolt, bullet or blast kills one.
- **Daud's assassins** fight with their left hand's gifts (`Twk_Inv_AssassinHand`): they
  teleport in a swirl to 3-8 m of Corvo (`DisTweaks_NPCTeleportSpell`), drag him towards them
  from afar (`DisTweaks_NPCAttractSpell`, 8-30 m, 4-8 s; a blink breaks free), holding their
  blades till he arrives for the coup de grace (`DisTweaks_NPCAttractSpellCoupDeGrace`: a big
  slash within 2.7 m, from any angle, that no sword lock can catch: its `m_bDisableVersus`), and shoot their wrist bows (`DisTweaks_NPCFireBow`,
  damage by difficulty).
- **Script-made pickups**: the level scripts' actor factories (`SeqAct_ActorFactory` + `DisActorFactoryTweakObj`) make runes, bone charms, coin pouches, elixirs and notes at their spawn points (the Hound Pits' tutorial rune, the Hub's rewards); each is cooked as a pickup with a hidden instance of its mesh that the script's spawn moves into place. Pickup names standing for their item ("`i", "`k") show the item's name.
- **Whale oil tanks** (`DisWhaleOilBattery` + `DisTweaks_WhaleOilBattery`): physics props seated in their receptacles; picked up, carried and thrown like any prop, they burst on a hard knock or a blow (their break step's `m_pExplosion`: 8 m, 50 damage), and a receptacle runs its device only while a tank sits in its seat. Holding a tank at an empty receptacle: "[F] Insert Whale Oil Tank". Explosions reach only what they can see.
- **Cinematic mode** (`SeqAct_ToggleCinematicMode`): the HUD hides (`bHideHUD`), Corvo is held (`bDisableMovement`/`bDisableInput`) and his arms hidden (`bHidePlayer`), each as the op says; released after a minute at most should a script never let go.
- **Attachments** (`SeqAct_AttachToActor`): emitters ride the character or prop they're attached to; triggers attached to a character move with it.
- **Level-script AI**: noises (`DisSeqAct_AINoise`, by loudness and context), distractions, suspicion, searches, guarding a place, scripted Whaler teleports, clearing and psychic attention, following someone, scripted shots (`DisSeqAct_AIShoot`: shots, accuracy), level swarms sent somewhere (`DisSeqAct_SetRatSwarmCustomBehavior`); an actor factory's "Spawned" is its spawn point (a PA speaker's announcements play from there); security circuitry panels ("[F] Rewire", with a Rewire Tool) rewire the nearest device of their kind. Brain flags (`DisSeqAct_AISetBrainFlags`, enabled, disabled or toggled: no attacking, no melee, no shooting, civilians who won't panic) hold the Flooded District assassins and others back; `DisSeqEvent_PlayerHeard` fires as a character hears Corvo, `DisSeqEvent_RatPossess` as he goes into a rat and out; `DisSeqAct_BodyShadowKill` marks characters whose bodies turn to ash as they die (Daud's Whalers).
- **Usable objects** (`DishonoredUsableObject` + `DisTweaks_UsableObject`, 626 across the maps: lockers, bins, chests, trunks, hatches, sewer plates, switches, crank wheels, valves, safes and their dials, security panels): the moving part is cooked as an animated type (its skeletal mesh and the tweak's anim sets, following the tweak's named fallback chain) and replaces its bind pose in the level; each use moves it into its next stage (`m_Stages`: the stage's sequence and sounds, its prompt override, `m_bDeactivateUsableWhenDone`) and raises its `DisSeqEvent_Used`; `DisSeqAct_ActivateUsable` sets a stage (`m_TargetStageIndex`) or the next. Kept in saves. Script: `usable [LABEL] [D]`.
- **Paths**: characters walk the original navigation meshes (each level's `DisPylon` `NavigationMeshBase`, version 28: vertices, the edge storage table, polygons, then `FNavMeshEdgeBase` / `FNavMeshPathObjectEdge` records in the table's order), cooked into `scene.navmesh`: A* over the polygons through their shared edges, then the funnel through those edges (kept clear of their ends) for the corners; 12 searches a frame, again when the goal moves. They open the unlocked doors in their way. `census` reports how many walk paths.
- **Level-script world actions**: patrols (`DisSeqAct_AISetPatrol`, from a route's nav point), senses (`DisSeqAct_AISetSenses`: blind, deaf), dispositions (`DisSeqAct_SetDisposition`: ally / neutral / enemy, reciprocated or one way, set, cleared or all cleared, between Corvo, factions and characters; kept in saves), `DisSeqAct_LimitPawnMinHealth`, `SeqAct_ChangeCollision`, the security devices (`DisSeqAct_WallofLightControl` off/on/toggle/polarity, `DisSeqAct_AlarmBell`, `DisSeqAct_PlugWhaleOilBattery`), `DisSeqAct_GivePickup` (its tweak's ammunition, coins, runes, charms, items and tutorial notes, cooked onto the op) and `DisSeqAct_GameOver` (the original's "Game Over" with the script's reason). Also object lists (`SeqAct_ModifyObjectList`, `SeqAct_IsInObjectList`), `SeqAct_Timer`, `DisSeqAct_GetAbstractItemQuantity` (how many rat viscera Corvo carries), `DisSeqAct_BendTime` (the scripts' own stopped time), `DisSeqAct_CancelPlayerActivePower`, `DisSeqAct_NPCMarkForVanish` (characters gone once out of sight), matinee fog tracks (`DisFogComponent.Opacity`, planes, height), `SeqAct_SetMaterial` (the new material cooked onto the op and made for each target surface at load, with that surface's lighting), `SeqAct_SetMatInstScalarParam` (the material instance's parameter slot, by its cooked name, in every material made from it: the Outsider's shrines fade in, Corvo's mark glows), `DisSeqAct_ToggleHUDElement` (the HUD's health, mana, equipment and crosshair: the Tower's opening shows none), `DisSeqAct_NPCTrackTarget` (heads follow Corvo), `SeqAct_GetDistance`, `DisSeqAct_Add/RemoveInventoryItem` (characters' swords and guns; Corvo starts the Tower unarmed and is handed a sword when the assassins strike), `DisSeqAct_Lock` on usable objects (and their keys), `DisSeqAct_SetIgnoreDeath`, `DisSeqAct_AIRingAlarm`, `DisSeqAct_RemoveAbstractItem`, `DisSeqAct_PlayerTrackTarget` (Corvo's view turned and held on someone), `DisSeqAct_EquipItemType` (empty hands in the Hound Pits: attacking or blocking draws the sword with its equip clip; a pistol, crossbow or the Heart selected), `DisSeqAct_AttachPickup` (a key hung on someone's belt, pickpocketable), `DisSeqAct_ToggleTutorial`, `DisSeqAct_SpawnCameraLensEffect` (the effect tweak's particle system cooked onto the op, held before the eye and turning with it at a third of UE3's 90-unit distance, nearer or further for the view's field of view as `EmitterCameraLensEffectBase` does (made for 80 degrees): rain on the lens, the Hound Pits sickness, water after a dive; looping ones run until "Stop Looping" or their life span, then "Finished"), `DisSeqAct_SetRainEmitter` (see Rain), matinee light `Brightness` tracks (the light's passes, its characters' passes and its Bevy light; the dominant sun's colour for the Lighthouse's lightning); events for line of sight (`SeqEvent_LOS`), patrol points reached, possession started / finished, props picked up, broken and knocked about (`SeqEvent_RigidBodyCollision`, over its `MinCollisionVelocity`), characters' behaviours starting (`DisSeqEvent_BehaviorStarted`: panic, patrol, react, search, combat), whale oil receptacles plugged / unplugged and characters calming down (`DisSeqEvent_AttentionDecreasedTo`). Named variables resolve within their own sequence first (each prefab's). Events take their
  class's trigger limit when the level leaves it unset (`MaxTriggerCount`, cooked from the
  class default: remote events and dialogue outputs are unlimited; at UE3's base of 1 a
  dialogue's second output and every repeated remote event were lost). Touch events answer
  characters too where `bPlayerOnly` is off (Emily reaching the gazebo, guards walking through
  a trigger, with them as the instigator), only the kinds `ClassProximityTypes` names (188 are
  for Corvo inside a creature, the possession proxy) and none `IgnoredClassProximityTypes`
  names; one turned on around Corvo fires at once (`bForceOverlapping`).
  `DisSeqCond_CompareTweaks` compares a character's pawn tweak (exact, or a variant);
  `DisSeqCond_IsDoorOpen`. Also `DisSeqAct_SetPlayerHealth` / `Mana`, `ModifyElixirCount`,
  `GiveUpgrade`, `RemoveKey`, `SetObjectiveHidden`, `TogglePowerWheel`,
  `SeqAct_AccessObjectList` and `DisSeqAct_TriggerExplosion` (the explosion tweak's blast
  cooked onto the op: the Prison's outer door). Also
  `SeqAct_SetPhysics` on props, `DisSeqAct_OverridePossess` (the possession tweak cooked onto
  the op through Arkane's tweak inheritance, `m_FallbackChainCooked` and `m_FallbackSkip`:
  Slackjaw and the bridge's guards can't be taken, the Art Dealer can, for so long, and Corvo
  comes out at the scene's exit point). Conditions: `DisSeqCond_IsSentinel` (false: the test harness's branch was swallowing the real one in 25 maps), `DisSeqCond_IsDLCUnlocked` (the installed `DLC/PCConsole/DLCnn`), `DisSeqCond_PawnIsPossessed`. `DH_SCRIPT_WORLD_LOG`.
- **Rain**: `DisSeqAct_SetRainEmitter` makes the level's rain emitter the camera's rain box, as the original's `DishonoredPlayerCamera` does. Arkane's native modules are reproduced: `DisParticleModuleRainDrops` keeps `m_NumRainDrops` drops in a box about the eye at the emitter's rate, with the module's start size, velocity (wind), colour and alpha, fading in at `m_FadingRate` and stretched by `ParticleModuleSizeMultiplyVelocity`. Each drop falls until it leaves the box or meets the highest surface over its 1 m column (found by rays from above, cached about the eye), where `DisParticleModuleRainImpacts` splashes at its emitter's rate. A roof over the eye sends the scripts "Rain Stop", and the open sky "Rain Start" after `m_fRainStartDelay`; the scripts put rain on the lens with those. Other rain systems draw nothing. `DH_RAIN_LOG`.
- **Rat repulsors** (`DisRatRepulsor`): swarms keep out of their radius, and the Devouring Swarm can't be summoned inside those that forbid it.
- **Wolfhound leaps** (`Twk_Inv_WolfhoundBite`): from 5.5-8 m a wolfhound springs at Corvo
  (`DisTweaks_WHJumpAttack`), knocking him down for `m_DamageOnKnockDown` (7/10/15/20 by
  difficulty) and pinning him (`DisTweaks_WHArmAttack`): its teeth take 5 every 0.75 s
  (`m_LoopSettings`) until he breaks free with 3 presses ("Mash [LMB] !", `DisTweaks_Minigame`).
  Up close they bite with their weapon's own moves (`DisTweaks_NPCAttackMedium`, `..Left90`,
  `..Right90`). Script `spawn NAME` starts a level-script spawner (a kennel's hounds).
- **Rats underfoot** (`DisBehaviorRatStomp`, `DisTweaks_NPCStomp` 2.5 m): people stamp on the rats that come near (their `..StompRat` clips and "rat" attack bark), killing those still under the heel.
- **Wall of light eyes**: the eye above each wall (`DisDetectionEye`) shows what it sees in its detection cylinder with its tweak's materials: white with nobody about, red at someone the wall would burn (with its detection sound), blue at someone it lets pass, dark when unpowered. A device's receptacle is the nearest of its name (sublevels reuse actor names). Script: `wall [D]`.
- **Tripwire traps**: the wires strung across passages (`DisTripwire`) snap when Corvo walks into them (jumping, Blinking or a small creature passes) and raise their `DisSeqEvent_Tripwire`; the level scripts fire the launchers (`DisSeqAct_ActivateProjectileLauncher`), which shoot at their target point on their sequence's fire notify: the whiskey launcher's explosive arrow bursts on what it strikes (`TrapArrowExplosion_twk`), the bolt launcher's bolt wounds. A launcher not yet fired can be disarmed for its ammunition (`m_HarvestedAmmo`: an incendiary bolt or a bolt). Sprung and disarmed traps, and broken or moved props, are kept in saves. Script: `trap [wire|launcher] [D]`; `DH_TRAP_LOG`.
- **Music boxes**: a musical Overseer fighting Corvo plays the protective tune, and so do the
  levels' tune sources (`DisProtectionTuneSource`): within 6 m the world sways, within 4 m
  Corvo's powers fail (`DisTweaks_Tune_Protection` radii and sounds, cooked from the music
  box's `DisTweaks_NPCTune_Protection`). Facing Corvo within 20 m and 45 degrees the Overseer
  plays its combat tune at him (`DisTweaks_NPCTune_Combat`, `Music_Amp_Damage`), whose
  musical damage (90 every 0.3 s) tears through the rats of a swarm in the cone; it stops a
  second after he leaves the cone, five after he's out of sight.
- **Physics props**: the levels' movables (`DishonoredMovable`: bottles, pans, cups, helmets)
  are physical, at rest until disturbed; the light ones are picked up ([Use]), dropped
  ([Use]) and thrown ([Attack], at `ThrowStrength`). Their knocks sound, spark and carry to
  the AI as the original contact system (`Dis_ContactSystem`) has it for their contact type
  against the world or a body; a hard enough knock (`m_fMinSpeedToBeDamaged`, or `...OnDrop`)
  breaks them (`m_Steps`: the sound, the AI noise, the effect, the pieces), and so do blades
  and bullets. The breakables in the way (`DishonoredBreakableNavBlock`: crates, planks) stay
  put until blades, bolts, bullets or blasts wear down their `m_Health`. Moved and broken
  props are kept in saves.
- **Breaking doors down** (`DisTweaks_Door`'s `m_Health`, `m_DamageThreshold` and
  `m_pBreakSteps`): Wind Blast and blasts wear down the wooden and glass doors (a blow under
  the threshold leaves no mark); at no health left the door's last `m_DoorBreakSteps` plays
  (the wood's or glass's sound, the AI's alarm, the splinters at its `brk` socket, the pieces
  flying) and the doorway is open. Broken doors stay broken in saves.
- **Taps and fountains** (`DisWaterSource`): [Use] "Drink" turns the valve and runs the water
  (the tap's own "Use" sequence: its sounds and stream); the Water of Life and Spirit Water
  charms make a drink give back health or mana (`WaterDrinkingHealthBonus`, `...ManaBonus`).
- **Locks and keys**: doors locked in the level (`m_bLocked`) stay shut ("Locked") until
  Corvo has one of their keys (`m_MatchingKeys`, by name: "Golden Cat Master Key"); keys
  come from key pickups (`DisKey_Base.m_Name`), pockets and the scripts
  (`DisSeqAct_AddKey`), go on his ring (kept in saves) and unlock the door on use.
- **Distractions**: patrollers stop at the levels' distractors (`DisNPCDistractor`: on
  their routes, by chance, with a cooldown) to play their scenes - the original soirees
  (`DistractionSoiree.*`: leaning on a railing, scavenging, a smoke, retching), their "Loop"
  repeated the distractor's count - then go on; anything alarming breaks it off. The level
  scripts hear of it (`DisSeqEvent_Distracted`) and run the props' scenes.
- **Keyholes**: a shut door whose mesh has a keyhole (`KeyHole` socket) shows "Hold [Use]
  Keyhole": a tap opens the door, a hold puts Corvo's eye to the keyhole (the view from the
  far side through the HUD movie's keyhole shape, at `m_fKeyHoleFOV`, the look held near
  straight through) until [Use] again; the level scripts' `DisSeqEvent_KeyHoleUsed` fire.
- **Mask optics**: [Zoom] (Alt) magnifies the view through the mask's lens once Corvo has the
  first optics upgrade, as the post-process graph's zoom lens draws it: `PPG_LensVectors` blurs
  and bends the lens's outer ring into the motion blur's field, `PPG_LensCompose` darkens the
  frame by `AltScreen_Effects.SpyGlass_Lens_01_m` (`MaskOpacity`), and the node's grading
  (neutral colour balance) comes in with the magnification; the second
  adds a stronger lens ([Use] while zoomed) and a distance read-out (`SpyglassDistance`).
  Aiming slows with the magnification, sprinting puts it away, with the original lens sounds.
- **Shader warm-up**: while the loading screen stays up, the world is drawn whole (no view
  culling) until every material's pipelines are compiled, so nothing appears late in play
  (`DH_NO_WARMUP=1` skips it).
- **Spline meshes**: the cables, pipes and rails of the levels' spline lofts
  (`SplineMeshComponent`) are their static meshes bent along each component's Hermite spline
  as UE3's spline mesh vertex factory does it (the mesh's Z along the spline, X and Y across
  in the frame `SplineXDir` sets, scale and roll interpolated), baked at cook time and lit by
  their own lightmaps.
- **Collision**: static meshes collide through their simplified collision hulls
  (`BodySetup.AggGeom`) where the original has them and `UseSimpleBoxCollision` is on, else
  through their triangles.
- **Effects**: the original Cascade particle systems, placed in the levels and spawned by
  gameplay (explosions, immolation, springrazor shrapnel, Shadow Kill ash, muzzle flashes),
  drawn with their translated shaders.
- **Dialogue**: the original dialogue trees (the one-shot dialogue actors' and each
  character's, through its voice): Kismet inputs, the player approaching, lingering or
  talking to a character ([F] Talk to Piero Joplin) enter through the trees' hooks, whose
  story-flag, conversation-already-had, time-limit and branch nodes pick what is said; lines
  are voiced from the speaker and subtitled with the character's name, and conversations set
  their story flags and raise their Kismet outputs. The player's answers in conversations
  (`DisConv_PlayerChoice`, 63 across the game: leaving the Hound Pits with Samuel, Piero's
  workshop, Slackjaw and Granny Rags, the Boyle sisters, the mask at the end) are offered like
  the scripts' choices, each option going on down its own branch (cooked flat, with jumps).
  The trees' story-flag and conversation-had nodes give their negative output first (a flag
  unset, a conversation not yet had). One-shot dialogue actors are spoken for by the
  characters the scripts name with them (`DisSeqAct_DialogInputs` Target): talking to the
  Empress runs her one-shot's "player used" hook, and a tree's remote nodes raise the
  speaker's dialogue outputs first (her "Give_Letter").
- **Scene animations**: a matinee group's own animation sets (`GroupAnimSets`: the
  Empress dying in Corvo's arms, Emily's hug, the Hound Pits' scenes) are added to the
  characters it binds, as UE3 adds them to the pawn; the player group's go to Corvo's arms,
  which play its keys while the scene runs. Characters change dress on the scripts' word
  (`DisSeqAct_NPCSetMaterials`: the Boyle sisters' red, black and white, by material slot).
- **Animated props**: skeletal level actors that matinees animate (the Tower's gangway
  lowering as the boat docks, machinery, doors in the Hound Pits; 29 maps) are cooked as
  animated types from their mesh and the scene group's `GroupAnimSets`, and play on the
  matinee's clock.
- **Posed props**: a skeletal level actor whose component holds an animation's frame
  (`Animations`: an AnimNodeSequence at its time) is cooked in that pose, mesh and physics
  bodies: the Tower's lowered gangway was a 4 m wall at its reference pose, and Corvo fell
  into the waterlock.
- **Scenes**: matinees move, show, hide and switch actors, cut cameras, fade, play the
  characters' animations and lines, and walk them to their marks (`InterpTrackLocomotion`, at
  the key's gait), turn them to face one another (`InterpTrackFaceTo`) and turn their heads to
  what they look at (`InterpTrackLookAt`), and hand them props (`InterpTrackAttachment`: a
  glass or cigarette rides the character's bone, smoke rides the cigarette, passengers ride
  the boat). Moves relative to an actor's start begin from where it stands (the Tower's
  boat is lowered from the dock, crosses the river and rises in the waterlock), and animated
  actors collide as their physics assets do (the boat's hull). A character speaking to Corvo looks at him, and a
  speaking character's face plays its line's FaceFX animation (below); lines without one move
  the jaw with their loudness (envelopes cooked by `cook-audio` into `audio/envelopes.bin`).
- **The Outsider's dream's tutorial steps**: the scripts put Corvo's things away
  (`DisSeqAct_BackupAndClearInventory`: all but his coins, with his upgrades) and give them
  back, open the journal on its powers page after the rune (`DisSeqAct_OpenJournal`) and go on
  once it has been seen and closed (`DisSeqEvent_JournalViewed`), wait on his mana
  (`DisSeqEvent_PlayerManaThreshold`) and hide the whole HUD (`SeqAct_ToggleHUD`). Objectives
  coming up raise `DisSeqEvent_TaskActivated` (the Bridge's markers), rat bites
  `DisSeqEvent_AttackedByRats`, blown-up tanks the battery's `Destroyed`. Characters beg on
  their knees or panic when the scripts say so (`DisSeqAct_AIDoSimpleBehaviors`: the
  original `Empty_Beg_Kneeling_*` clips), and the scripts put things in pockets for Corvo to
  steal (`DisSeqAct_SpawnStealable`: the Flooded District's elixir pouches, Granny Rags' sewer
  key).
- **The golden rim**: what Corvo can use, in reach and looked at (a pickup, a door, a lever,
  a valve), is drawn over with the interactables' highlight material
  (`vfx_interactivity.interactivity_INST`, `golden_focus_PMAT`'s fresnel glow with the
  instance's colour, power and visibility; its shader map has no vertex shader to pair the
  original pixel shader with, so `highlight.wgsl` does its math), as the original's highlight
  meshes are; the level scripts highlight things of their own
  (`DisSeqAct_Highlight`).
- **The Hound Pits remember**: what Corvo takes there stays taken on his later visits (the
  hub's level state, `DisSeqAct_SaveLevelState`): its pickups by actor and place across the
  hub's map variants, kept in the campaign and in saves.
- **Level streaming**: sublevels the scripts stream (`LevelStreamingKismet`: the Hound Pits'
  scripts for each return from a mission, high-chaos variants, the ending's set dressing) stay
  out (scripts asleep, characters, props, pickups, water and blocking volumes absent) until
  the campaign's map change names them (`InitiallyLoadedSecondaryLevelNames`) or a streaming
  op loads them.
- **Campaign**: the original persistent level's scripts (`DishonoredGameFull_P`) run alongside
  every map, as in the original: the maps' `ChangeLvl_*` events lead to the mission statistics
  screens (each mission's rows, story outcomes and illustration), chapter notes, story flags
  and the map change, where the chaos level picks the high or low chaos version of a mission
  and the story flags pick the ending. Save games keep it all.

- **Faces (FaceFX)**: the dialogue data's `FaceFXAnimSet`s and the heads' `FaceFXAsset`s
  are decoded (FaceFX archive 1731: the lines' curves by target — `open`, `W`, `PBM`, `wide`,
  tongue, head orientation, gaze, blinks — keyed by the voice event's id; each character's
  compiled face graph, its nodes summing their inputs through link functions, and the face
  bones' deltas per bone-pose node) into `cache/facefx/<map>.json`. A spoken line's curves run
  through the speaker's graph and move its jaw, lips, tongue, lids and brows over its
  animation (`DH_FACEFX_LOG`, `DH_FACEFX_FORCE=NODE`).
- **Movies**: the Bink movies are converted at cook time (ffmpeg's Bink decoder) into MJPEG
  frames and a WAV soundtrack (`cache/movies`, `dhtool cook-movies`). New Game plays the
  intro (the Empress's letter, its subtitles from `Movies/INTRO_LOC.txt` and
  `Subtitles.int`; skippable) while the Tower loads; each map's loading screen loops its
  `m_LoadingMovieName` (`DefaultEngine.ini`) behind the title and hints; the scripts'
  `SeqAct_ControlGameMovie` plays the Tower's title card and the endings' credits.
- **Collision**: static meshes collide as their simplified collision says (`AggGeom`: boxes,
  spheres, capsules and convex hulls; the Prison cells' floors are boxes).
- **Doors** start as the level leaves them (`m_InitialDoorState`: the Prison cell's is open
  for the interrogation) and tell their scripts when they have swung shut or open (the cell
  locks behind Corvo once closed; the key from the food tray opens it, unlocking and swinging
  it at once). A door swings away from whoever opens it across its face (the mesh's thin
  axis), so corridors stay clear on the far side.
- **The Prison plays through**: the walkway key (on whichever guard's spawner carries it,
  `m_pStealablePickup`; markers follow it on his belt, and a task's several targets
  `m_TargetIndex` all show), the Interrogation Room, the explosive, the main gate (its lever's
  matinee carries the gate's crusher, attached by `Base`), the outer door blown open, the
  dive and swim to the sewers and on to `L_PrsnSewer_P`.
- **Touch volumes** fire on Corvo's whole capsule (as UE3's cylinder does), not his middle:
  flat triggers on the floor work.
- **Saves** keep the scripts' cinematic mode and the HUD parts they hid (the Prison's health
  bar after waking), and put matinee movers' colliders back with them (a gate left open stays
  open to walk through).
- **Particles**: mesh emitters' type data comes from the LOD's `TypeDataModule` (with its
  fixed Pitch / Yaw / Roll), and local-space emitters carry their particles when what they
  follow moves or turns; axis-locked sprites (`ParticleModuleOrientationAxisLock`
  EPAL_X..) lie in the plane UE3's `GetAxisLockValues` gives them with size X along its up
  (the near-death glow fills the view instead of a band down its middle).
- **Pickups**: notes take their text from their tweak's abstract item (the bread note opens in
  the journal); weapons (`DishonoredInventoryPickup`: the Prison's guard sword, pistols,
  grenades, springrazors, the Flooded District's blades) give what they are; the scripts
  listening to everything Corvo uses (`DisSeqEvent_Interact` without an originator) hear what,
  and compare its tweak lineage (`DisSeqCond_CompareTweaks` "Fallback Related": the sword is
  an `InventoryPickupSwordBase`) for "Take a weapon".

## Workspace layout

| Crate | Purpose |
| --- | --- |
| `crates/upk` | UE3 package reader: LZO/zlib chunks, name/import/export tables, tagged properties, static/skeletal meshes, textures (incl. `.tfc` bulk data) |
| `crates/dhcook` | Cooker: levels and their streaming sublevels, materials and shaders, lightmaps, light volumes, NPC archetypes and factions, Kismet/Matinee, security devices, Wwise audio, Scaleform UI, game data |
| `crates/dhtool` | CLI for cooking and inspecting packages (`cook`, `cook-all`, `cook-ui`, `cook-audio`, `cook-gamedata`, `list`, `props`, `findprop`, `swfdump`, `mat`, `graph`, ...) |
| `crates/game` | The Bevy game |

### Cache format (`./cache`)

Everything the game needs is converted ahead of time into formats it can upload directly:

- `maps/<map>.json`: the scene (instances with world transforms, materials, lights, spawners,
  patrol routes, pickups, volumes, fog, NPC types and skeletons, level scripts, security).
- `meshes/*.mesh`: little-endian vertex/index arrays per mesh, in Bevy's coordinate system
  (Y-up, metres).
- `textures/*.tex`: block-compressed mip chains.
- `anims/*.anim`, `audio/`, `ui/`, `game/gamedata.json`, `game/campaign.json` (the campaign
  scripts).

## Automated testing

`DH_SCRIPT` drives the game with scripted input (it never grabs the OS cursor or reads real
input), for example:

```sh
DH_SCRIPT="wait 2; behind lone; attack; wait 1; log; shot kill; exit" ./target/release/dishonored
```

Commands: `wait S`, `goto X Y Z [S]` (walk there over the level's navmesh, using, swinging at
or jumping onto what holds it up; `DH_GOTO_LOG`), `gototask [N] [S]` (to the N-th objective marker), `tp/fly X Y Z YAW PITCH`, `look YAW PITCH`, `lookat X Y Z`, `walk S`,
`attack`, `block S`, `key K`, `hold K S`, `rmb`, `rmbhold S`, `mmbhold S`, `behind [N|lone]`,
`front N`, `tpnpc N [NAME]`, `npcidle` (the nearest hostile character stands still facing Corvo), `health N` (set his health: the near-death effects), `hurt D ANGLE` (a blow of D from ANGLE degrees off his view, clockwise: the damage feedback), `aware L1,L2,..` (the awareness markers step through attention levels, 1.5 s each), `location NAME` (the location banner; `_` for spaces), `message TEXT` / `tutorial TEXT` (a HUD game message, a tutorial), `objpopup STATE NAME|TASK[:STATE]|..` (the objectives popup), `breath N` (seconds of breath left: the oxygen gauge), `krust [D] [N]` (hover in front of a river krust), `possess fish|krust|rat`, `prop [NAME]` (stand at a prop), `weapons`, `explode [D]`, `flare [D]`, `select POWER`, `pickup`, `door`, `use`, `god`, `epp Epp_NAME [0|1]` (a scripted screen effect: Knocked, Weepers), `giveupgrade Twk_Upgrade_NAME`, `kop OP [INPUT]` (activate a level script op: test one step of a sequence), `campos X Y Z YAW PITCH` (hold the view at a point; `campos -` lets go), `givepower NAME LEVEL`, `runes N`,
`charms N`, `mana`, `ammo TYPE N` (0 bullets .. 6 grenades, 7 sticky), `fx EFFECT [DIST]`,
`remote EVENT` (a level-script remote event, e.g. `ChangeLvl_fromStreets1_toHub`), `chaos N`,
`ko [NAME]` (a sleep dart into the nearest, or the nearest whose name or pawn holds NAME),
`side [N|lone|NAME]` (3 m off a character's right), `face [NAME]` (aim at the nearest
character), `save N` / `load N`,
`travel MAP`, `log` (position, stats, powers, nearby NPCs), `census`, `shot NAME`, `exit`. `DH_NOVSYNC=1` disables vsync for benchmarking; `cache/sweep.sh` loads
and screenshots every map. `camat NAME [D] [YAW]` holds the view on a character's face (the one speaking, from
Corvo's side; `camat -` lets go). `DH_INTRO=1` keeps the new game's intro in scripted runs;
`DH_MOVIE_SHOT=SECS:PATH` screenshots a movie that far in; `DH_NPC_CLIP_LOG` logs the clips
of the characters near Corvo; `DH_ANIM_CHECK` how each anim set binds to its skeleton. `kuwa` (or
`DH_KUWA=1`) turns on the painterly branch; `DH_AUDIO_LOG` logs the events posted and, each
second, how many sounds play; `DH_RTPC_LOG` the sounds whose RTPC curves change them.
`dhtool mapshader <map> [exprs|VF TYPE]` prints a material shader map's uniform expressions
or one of its shaders translated to WGSL (`-` as the vertex factory: its non-mesh shaders);
`dhtool findmap NAME` lists the shader maps of materials by name, `dhtool mattex PKG MAT` the
textures a material's compiled resource samples.
