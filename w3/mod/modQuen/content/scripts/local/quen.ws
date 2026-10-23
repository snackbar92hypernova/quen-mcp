// modQuen: the script half of quen's Witcher 3 package (quen-mcp w3\README.md), with
// W3Quen.asi. Needs in bin\x64_dx12: an ASI loader, WitcherNativeBridge.asi (public 2.0) and W3Quen.asi. It imports
// W3Quen.asi's natives, so without them the script compile fails at boot: remove this folder first, then the ASI.
//
// Five parts, all quiet outside quen sessions (the ASI writes and serves nothing without quen's launch token):
// - Game state: from CR4Game.OnTick, at most 4 times a second by local time, QUEN_State(flags) whenever the flags
//   change; the ASI writes each to W3QuenState.log. Also at once when a game starts (OnGameStarted,
//   OnAfterLoadingScreenGameStart) and when the photo-mode menu opens or closes. OnGameEnded sends 0 at once: a load or
//   the main menu begins, and the tick is silent through a load's loading screen (17-18 s with no state at live boot 1).
//     flags  0 no player (main menu, between saves); else 1 (valid) plus
//           16 a non-gameplay scene plays        32 the player is in a non-gameplay cutscene
//          128 photo mode                       256 blackscreen, fading or the loading video
//          512 the script flag IsDialogOrCutscenePlaying (set when a blocking scene starts; can go stale)
//         2048 a menu is open (the GUI manager's IsAnyMenu: the pause menu the game opens when it loses the front,
//              among others; from quen-w3 0.1.6)
//   The W3 Game State Bridge's numbers, from the same vanilla calls, so quen's parser reads both; 2048 is quen's own.
// - The command channel: QUEN_Next hands one command per tick, already checked by the ASI, as
//   "<handle> <verb> <args>": numbers split by single spaces, or one name that may hold spaces. Every command gets
//   exactly one QUEN_Reply in the same call. Its data is read-back the ASI turns into JSON: listsaves "a|b|c",
//   cam.get, cam.set, cam.path and cam.frame "<camera> x y z pitch yaw roll fov". The ASI guarantees each verb's
//   number count; a script error mid-command comes back to quen as "no reply" at the next poll. save (from quen-w3
//   0.1.4) writes a new manual save as the menu's "Empty save slot" does; the game names the file. world.get (from
//   quen-w3 0.1.5) reads back "x y z heading day hour rain snow skyclear weather" (rain, snow and skyclear from quen-w3
//   0.1.9) and changes nothing. notice (from quen-w3 0.1.8) shows quen's text on screen for 8 s, as QUEN_Say does, and
//   changes nothing; from quen-w3 0.1.15 for the time quen gives, with notice.hide to take it down and sound to play a
//   sound event; from quen-w3 0.1.16 a lasting one on quen's own HUD module where the HUD draws it (QUEN_Show).
//   items.add (from quen-w3 0.1.10) adds an item by its name, as the console's additem does, equips it
//   if asked, and reads back "before after equipped": the item's count before and after, and 0 or 1. world.actors (from
//   quen-w3 0.1.10) reads back the actors near the player (QUENRPC_Actors) and changes nothing; from quen-w3 0.1.12 by
//   tag or class, each with its id. From quen-w3 0.1.13 four more reads that change nothing: scene.frame, what the
//   rendered camera sees (QUENRPC_Frame); scene.ground, the ground at points (QUENRPC_Ground); scene.ray, rays and
//   sphere sweeps (QUENRPC_Ray); nav.line, navmesh lanes (QUENRPC_Nav); and cam.noclip, the photo-mode camera's no-clip
//   (put back as photo mode's menu has it when the menu closes). The exec verb (0.1.13) never reaches the script:
//   W3Quen.asi runs it. From quen-w3 0.1.17 the game's own free camera, outside photo mode: freecam turns it on or
//   off, freecam.set moves it, freecam.get reads its state (QUENRPC_FreeCam), which each of them reads back.
// - Photo mode (from quen-w3 0.1.9): photo.mode opens it by PhotomodeManager's own EnablePhotomode and closes it by its
//   menu's own exit; photo.get, photo.weather, photo.env, photo.time, photo.grade, photo.reset, photo.tick and photo.list
//   call CR4PhotomodeEffects as photo mode's menu does (photomodeMenu.ws OnParameterChanged), only while that menu is
//   open: it turns the effects on as it opens and off as it closes. Every set reads back the whole photo state.
// - Camera paths: cam.path loads one on the photo-mode camera, played from CR4Game.OnTick, one
//   set per tick: real time, or frame step (cam.frame sets frame n, then holds). W3Quen.asi checks and interpolates the
//   path as quen-core sweep.rs does, in double precision (its offline test checks sweep.rs's reference vectors), and
//   times real time by the performance counter; QUEN_CamAt gives the pose for this tick.
// - Console commands, which quen types only when the command channel isn't up:
//     quenload(FILENAME)  loads the save whose filename is FILENAME (case-insensitive, ".sav" optional).
//                         Refuses, with the reason on screen, on 0 or more than 1 match.
//     quenlast()          loads the newest save, like the menu's Continue.
//     quensaves()         logs every save filename and shows the count and the first 20 on screen.
//
// On 5.01 CR4Game.OnTick runs about once per frame in gameplay, in photo mode, under theGame.Pause and SetTimeScale(0)
// and behind the pause menu (a probe boot, 2026-10-17); whether it runs at the main menu is open (the ASI logs how many
// polls come before the first state). Every wrap calls wrappedMethod() first and depends on no other mod's wraps.
// Every name starts with QUEN or quen: the Native Bridge keeps the first registration of a name, and a script name
// another mod also declares is a compile error.
//
// The menu's own load (content0 game\gui\main_menu\ingameMenu.ws:3581-3724) runs CheckModdedSave (mod.io) and,
// in some cases, a confirmation popup before theGame.LoadGameInit. These functions call the natives directly and
// skip both. Matching follows exec function StartNewGamePlus (game\temp.ws:11328).
import function QUEN_Next() : string;
import function QUEN_Reply(handle : int, ok : bool, text : string, data : string);
import function QUEN_State(flags : int);
import function QUEN_CamAt(frame : int) : string;

@addField(CR4Game) var quenStateDue : int;   // GetLocalTimeAsMilliseconds of the next state poll
@addField(CR4Game) var quenStateSent : int;  // the flags last sent
@addField(CR4Game) var quenCamPath : int;    // the camera path loaded: 0 none, 1 real time, 2 frame step
@addField(CR4Game) var quenCamFrame : int;   // frame step: the frame shown
@addField(CR4Game) var quenCamFrames : int;  // frame step: how many frames the path has
@addField(CR4Game) var quenPhotoMenu : CR4PhotomodeMenu;  // photo mode's menu while it is open, else NULL
@addField(CR4Game) var quenGradeOpen : array<float>;     // the grading sliders (QUEN_Grade) as that menu opened
@addField(CR4Game) var quenNoclip : bool;  // cam.noclip turned the photo-mode camera's no-clip on
@addField(CR4Game) var quenHud : CR4ScriptedHud;  // the HUD whose module table holds quen's line (QUEN_HudModule)
@addField(CR4Game) var quenHudReady : bool;       // that HUD got UpdateHUD() once quen's module had loaded
@addField(CR4Game) var quenLine : string;         // quen's lasting line (a notice of a minute or more), "" when none
@addField(CR4Game) var quenLineMs : float;        // its time

@wrapMethod(CR4Game) function OnTick()
{
	wrappedMethod();
	QUEN_PollState(false);
	QUEN_CamTick();
	QUENRPC_Poll();
}

// The first state of a game comes from here, not one tick later (live boot 2: quen's first state came 0.33 s after the
// W3 Game State Bridge's, which polls here).
@wrapMethod(CR4Game) function OnGameStarted(restored : bool)
{
	wrappedMethod(restored);
	QUEN_PollState(true);
}

@wrapMethod(CR4Game) function OnAfterLoadingScreenGameStart()
{
	wrappedMethod();
	QUEN_PollState(true);
}

@wrapMethod(CR4Game) function OnGameEnded()
{
	wrappedMethod();
	quenStateSent = 0;
	quenStateDue = 0;
	QUEN_State(0);
	// A load or the main menu: quen sends its line again into the next game, whose HUD has no table entry yet.
	quenLine = "";
	quenHud = NULL;
}

// Photo mode switches on before its menu opens and off before it closes (photomodeManager.ws), so these polls see the
// new state. Live boot 2: quen's log never showed photo mode (128) while the bridge, which polls from the menu too, did.
// ponytail: no poll from the menu's own OnTick: CR4Game.OnTick, which polls the state and the channel, runs in photo
// mode (~51/s in the probe boot). Wrap the menu's OnTick too if a live boot shows otherwise.
@wrapMethod(CR4PhotomodeMenu) function OnMenuShown()
{
	wrappedMethod();
	theGame.QUEN_PhotoMenu(this);
	theGame.QUEN_PollState(true);
}

@wrapMethod(CR4PhotomodeMenu) function OnClosingMenu()
{
	wrappedMethod();
	theGame.QUEN_NoclipBack();
	theGame.QUEN_PhotoMenu(NULL);
	theGame.QUEN_PollState(true);
}

// Photo mode's menu's own Noclip setting (its Camera tab): what OnParameterChanged gives SetNoclip.
@addMethod(CR4PhotomodeMenu) function QUEN_Noclip() : bool
{
	return m_UIValues.Contains(PM_Camera_Noclip) && m_UIValues[PM_Camera_Noclip] == 1;
}

// After cam.noclip on: the photo-mode camera's no-clip back to the menu's setting (off without the menu: its default
// as it opens, photomodeMenu.ws OnConfigUI). Nothing otherwise.
@addMethod(CR4Game) function QUEN_NoclipBack()
{
	if (!quenNoclip)
		return;
	quenNoclip = false;
	if (quenPhotoMenu)
		GetPhotomodeCamera().SetNoclip(quenPhotoMenu.QUEN_Noclip());
	else
		GetPhotomodeCamera().SetNoclip(false);
}

// ---- photo mode ----

// Photo mode's menu as it opens (its OnMenuShown turned the effects on) and the grading then, or NULL as it closes (its
// OnClosingMenu turned them off). The menu takes its own slider defaults from the same getters as it opens.
@addMethod(CR4Game) function QUEN_PhotoMenu(menu : CR4PhotomodeMenu)
{
	quenPhotoMenu = menu;
	if (menu)
		quenGradeOpen = QUEN_Grade(GetPhotomodeEffects());
}

// EnablePhotomode (private) after vanilla's own conditions for it, as its key calls it: "" when it ran, else the
// conditions that hold. EnablePhotomode returns without a word on any of them (photomodeManager.ws).
@addMethod(PhotomodeManager) function QUEN_Enable() : string
{
	var why : string;

	if (m_photomodeEnabled)
		why += ", its manager has it on";
	if (theGame.IsDialogOrCutscenePlaying())
		why += ", a dialogue or cutscene";
	if (!thePlayer.IsAlive())
		why += ", the player is dead";
	if (thePlayer.IsInCutsceneIntro())
		why += ", a cutscene intro";
	if (theGame.IsBlackscreenOrFading() || theGame.HasBlackscreenRequested())
		why += ", a blackscreen or fade";
	if (theGame.IsCurrentlyPlayingNonGameplayScene())
		why += ", a non-gameplay scene";
	if (theGame.GetGuiManager().IsAnyMenu())
		why += ", a menu is open";
	if (theGame.GetTutorialSystem().HasActiveTutorial())
		why += ", a tutorial shows";
	if (why != "")
		return StrAfterFirst(why, ", ");
	EnablePhotomode();
	return "";
}

// The 8 grading sliders of photo mode's Effect and Overlay tabs, in photo.grade's order: exposure, contrast, highlights,
// saturation, temperature (kelvin: the menu's steps are BinToTemperature's), vignette, film grain, chromatic aberration.
function QUEN_Grade(fx : CR4PhotomodeEffects) : array<float>
{
	var g : array<float>;

	g.PushBack(fx.GetExposure());
	g.PushBack(fx.GetContrast());
	g.PushBack(fx.GetHighlights());
	g.PushBack(fx.GetSaturation());
	g.PushBack(fx.GetTemperature());
	g.PushBack(fx.GetVignette());
	g.PushBack(fx.GetFilmGrain());
	g.PushBack(fx.GetChromaticAberration());
	return g;
}

function QUEN_SetGrade(fx : CR4PhotomodeEffects, i : int, v : float)
{
	switch (i)
	{
		case 0: fx.SetExposure(v); break;
		case 1: fx.SetContrast(v); break;
		case 2: fx.SetHighlights(v); break;
		case 3: fx.SetSaturation(v); break;
		case 4: fx.SetTemperature(v); break;
		case 5: fx.SetVignette(v); break;
		case 6: fx.SetFilmGrain(v); break;
		case 7: fx.SetChromaticAberration(v); break;
	}
}

// photo.*'s read-back, 4 decimals: "<time> <8 grading> <8 grading as the menu opened> <rain> <snow> <skyclear 0|1>|<photo
// weather>|<env path, or empty>|<world weather>". The time is GetTimeOfDay's; the menu's slider passes minutes of the day.
@addMethod(CR4Game) function QUENRPC_PhotoState() : string
{
	var fx : CR4PhotomodeEffects;
	var env : CEnvironmentDefinition;
	var g : array<float>;
	var text, path : string;
	var i : int;

	fx = GetPhotomodeEffects();
	g = QUEN_Grade(fx);
	text = FloatToStringPrec(fx.GetTimeOfDay(), 4);
	for (i = 0; i < 8; i += 1)
		text += " " + FloatToStringPrec(g[i], 4);
	for (i = 0; i < 8; i += 1)
		text += " " + FloatToStringPrec(quenGradeOpen[i], 4);
	text += " " + QUENRPC_Env();
	fx.GetEnvDef(env);
	path = "";
	if (env)
		path = env.GetPath();
	return text + "|" + fx.GetWeather() + "|" + path + "|" + NameToString(GetWeatherConditionName());
}

// The blended weather values the scripts can read, "<rain> <snow> <skyclear 0|1>" (environment.ws; the HUD's weather icon
// reads the same three, environmentManager.ws GetCurWeather). Nothing in the scripts reads the environment's blend weight.
function QUENRPC_Env() : string
{
	var text : string;

	text = FloatToStringPrec(GetRainStrength(), 4) + " " + FloatToStringPrec(GetSnowStrength(), 4);
	if (IsSkyClear())
		return text + " 1";
	return text + " 0";
}

// With `now`, polls at once; else at most every 250 ms.
@addMethod(CR4Game) function QUEN_PollState(now : bool)
{
	var t : int;
	var flags : int;
	var gui : CR4GuiManager;

	// Local time, not GetEngineTimeAsSeconds: the engine clock likely stands still in photo mode (live boot 2's missing
	// 128), and moves in 31.25 ms steps (the probe boot). The pause menu times its 300 ms debounce with this one (ingameMenu.ws).
	// A due time more than 1 s ahead is stale (the clock jumped back), so poll then too.
	t = GetLocalTimeAsMilliseconds();
	if (!now && t < quenStateDue && quenStateDue - t <= 1000)
		return;
	quenStateDue = t + 250;

	flags = 0;
	if (thePlayer)
	{
		flags = 1;
		if (IsCurrentlyPlayingNonGameplayScene())
			flags += 16;
		if (thePlayer.IsInNonGameplayCutscene())
			flags += 32;
		if (GetPhotomodeEnabled())
			flags += 128;
		if (IsBlackscreenOrFading() || IsLoadingScreenVideoPlaying())
			flags += 256;
		if (IsDialogOrCutscenePlaying())
			flags += 512;
		// The vanilla check before photo mode opens (photomodeManager.ws) and before the pause key opens the pause menu
		// (playerInput.ws OnIngameMenu). OnTick runs behind the focus-loss pause menu (the probe boot), so this poll sees that one.
		gui = GetGuiManager();
		if (gui && gui.IsAnyMenu())
			flags += 2048;
	}
	if (flags != quenStateSent)
	{
		quenStateSent = flags;
		QUEN_State(flags);
		// From quen-w3 0.1.16: photo mode, a menu or a scene in or out, quen's lasting line moves between its HUD
		// module and the notification (QUEN_Show).
		if (quenLine != "")
			QUEN_Show(quenLine, quenLineMs);
	}
}

// ---- quen's line ----

// quen's own HUD module (from quen-w3 0.1.16): Mods\modQuen's bundle holds its movie,
// gameplay\gui_new\swf\hud\hud_quen.redswf (quen-mcp w3\hud\quen\QuenHud.as), registered under the HUD's dormant
// WatermarkModule: vanilla never creates it, its script class has no OnTick, its anchor sits at the top centre. The
// entry is added at run time on the HUD root's moduleManager, as Hud.as adds the others, then DiscardHudModule and
// CreateHudModule (the indicator probe boot, 2026-10-21: no vanilla file or wrap). Its movie loads on a later tick.
// Returns the module's sprite once loaded, NULL until then; with load, adds the entry first on a HUD without it.
@addMethod(CR4Game) function QUEN_HudModule(load : bool) : CScriptedFlashSprite
{
	var hud : CR4ScriptedHud;
	var manager : CScriptedFlashObject;
	var add : CScriptedFlashFunction;
	var module : CHudModule;
	var s : CScriptedFlashSprite;

	hud = (CR4ScriptedHud)GetHud();
	if (!hud || !hud.GetHudFlash())
		return NULL;
	if (hud != quenHud)
	{
		if (!load)
			return NULL;
		quenHud = hud;
		quenHudReady = false;
		manager = hud.GetHudFlash().GetMemberFlashObject("moduleManager");
		if (manager)
			add = manager.GetMemberFlashFunction("AddEntry");
		if (!add)
			return NULL;
		// Hud.as's own depth for it (its commented-out entry).
		add.InvokeSelfThreeArgs(FlashArgString("WatermarkModule"), FlashArgString("hud_quen.swf"), FlashArgInt(10));
		hud.DiscardHudModule("WatermarkModule");
		hud.CreateHudModule("WatermarkModule");
		return NULL;
	}
	module = hud.GetHudModule("WatermarkModule");
	if (module)
		s = module.GetModuleFlash();
	if (!s || !s.GetMemberFlashFunction("SetText"))  // not loaded yet, or another mod's movie in the slot
		return NULL;
	if (!quenHudReady)
	{
		// A module that loads late comes up in its Flash Hide state: the current input context, sent again, gives it
		// that context's state (the probe boot: OnDemand in Exploration).
		quenHudReady = true;
		hud.UpdateHUD();
	}
	return s;
}

// Whether the HUD draws quen's module now: its root shown (the user's HUD toggle and a quest's ShowHud hide it, hud.ws
// ForceShow), no photo mode and no menu (neither draws the HUD, while its Flash still reads shown: the probe boot), and
// the module out of its Flash Hide state (scenes, the radial menu and death hide it: Hud.as hudModuleStateArray).
@addMethod(CR4Game) function QUEN_HudDrawn(s : CScriptedFlashSprite) : bool
{
	var gui : CR4GuiManager;
	var states : CScriptedFlashObject;

	gui = GetGuiManager();
	if (!s || GetPhotomodeEnabled() || !gui || gui.IsAnyMenu() || !GetHud().GetHudFlash().GetVisible())
		return false;
	states = s.GetMemberFlashObject("stateMachine");
	return states && states.GetMemberFlashString("current") != "Hide";
}

// quen's module hidden within the call in any state (the probe boot: the next tick shows nothing): ShowElement(false,
// true) as hudModuleBase.ws sends it, so a context change doesn't bring it back, then again past the state.
@addMethod(CR4Game) function QUEN_HudHide()
{
	var s : CScriptedFlashSprite;
	var show : CScriptedFlashFunction;

	s = QUEN_HudModule(false);
	if (!s)
		return;
	show = s.GetMemberFlashFunction("ShowElement");
	show.InvokeSelfTwoArgs(FlashArgBool(false), FlashArgBool(true));
	show.InvokeSelfThreeArgs(FlashArgBool(false), FlashArgBool(true), FlashArgBool(true));
}

// quen's line on screen for ms: a lasting one (a minute or more: the module has no timer) on quen's HUD module where
// the HUD draws it, the notification taken down when it holds quen's line; else on the overlay popup's notification,
// as to 0.1.15 (it shows in gameplay, photo mode and over the pause menu: the probe boot), the module hidden. The
// reply: "notice already shown" when the line is up there with this text (quen's re-send changes nothing on screen),
// "notice shown", or "" when it can't be shown (no HUD module drawn and no overlay popup).
@addMethod(CR4Game) function QUEN_Show(text : string, ms : float) : string
{
	var gui : CR4GuiManager;
	var popup : CR4OverlayPopup;
	var note, s : CScriptedFlashSprite;
	var field : CScriptedFlashTextField;
	var noted : string;

	gui = GetGuiManager();
	if (gui)
		popup = (CR4OverlayPopup)gui.GetPopup('OverlayPopup');
	if (popup && popup.GetPopupFlash())
		note = popup.GetPopupFlash().GetChildFlashSprite("notificationModule");
	if (note)
		field = note.GetChildFlashTextField("tfMessage");
	// OverlayPopupMenu.as's notificationModule and its tfMessage (the probe boot's reads).
	if (field && note.GetVisible() && note.GetAlpha() > 0)
		noted = field.GetText();
	if (ms >= 60000)
		s = QUEN_HudModule(true);
	if (QUEN_HudDrawn(s))
	{
		if (noted != "" && (noted == text || noted == quenLine))
			popup.ClearNotificationsQueue();
		field = s.GetChildFlashTextField("tfText");
		if (field && s.GetVisible() && s.GetAlpha() > 0 && field.GetText() == text)
			return "notice already shown";
		s.GetMemberFlashFunction("SetText").InvokeSelfOneArg(FlashArgString(text));
		s.GetMemberFlashFunction("ShowElement").InvokeSelfTwoArgs(FlashArgBool(true), FlashArgBool(true));
		return "notice shown";
	}
	QUEN_HudHide();
	// Only on the overlay popup there is: without one, the GUI manager's ShowNotification requests it with the text,
	// which may then show after a notice.hide found none to hide.
	if (!popup || !popup.GetPopupFlash())
		return "";
	if (noted == text)
		return "notice already shown";
	// queue false: quen's replaces a notification on screen (OverlayPopupMenu.as showNotification).
	popup.ShowNotification(text, ms, false);
	return "notice shown";
}

// ---- camera paths ----

// Sets the photo-mode camera to the loaded path's pose for this tick: real time (frame -1: the ASI's clock since the
// path's first pose) or frame quenCamFrame. Leaving photo mode unloads the path. False: nothing was set.
@addMethod(CR4Game) function QUEN_CamTick() : bool
{
	var n : array<float>;

	if (quenCamPath == 0)
		return false;
	if (!GetPhotomodeEnabled())
	{
		quenCamPath = 0;
		return false;
	}
	if (quenCamPath == 2)
		QUENRPC_Numbers(QUEN_CamAt(quenCamFrame), n);
	else
		QUENRPC_Numbers(QUEN_CamAt(-1), n);
	if (n.Size() != 7)
	{
		quenCamPath = 0;  // the ASI has no pose for it: never expected
		return false;
	}
	QUENRPC_SetCam(n);
	return true;
}

// ---- saves ----

function QUEN_Say( text : string )
{
	LogChannel( 'Quen', text );
	theGame.GetGuiManager().ShowNotification( text, 8000 );
}

// Lower case, without a trailing ".sav".
function QUEN_Key( filename : string ) : string
{
	var key : string;

	key = StrLower( filename );
	if ( StrEndsWith( key, ".sav" ) )
	{
		key = StrLeft( key, StrLen( key ) - 4 );
	}
	return key;
}

function QUEN_LoadSave( filename : string ) : bool
{
	var saves : array< SSavegameInfo >;
	var key : string;
	var i, hit, hits : int;

	key = QUEN_Key( filename );
	hits = 0;
	theGame.ListSavedGames( saves );
	for ( i = 0; i < saves.Size(); i += 1 )
	{
		if ( QUEN_Key( saves[i].filename ) == key )
		{
			hit = i;
			hits += 1;
		}
	}
	if ( hits != 1 )
	{
		QUEN_Say( "quen: " + hits + " of " + saves.Size() + " saves match " + filename + ", not loading" );
		return false;
	}
	QUEN_Say( "quen: loading " + saves[hit].filename );
	theGame.LoadGameInit( saves[hit], false );
	return true;
}

function QUEN_LoadLast()
{
	QUEN_Say( "quen: loading the last save" );
	theGame.LoadLastGameInit();
}

function QUEN_ListSaves()
{
	var saves : array< SSavegameInfo >;
	var text : string;
	var i : int;

	theGame.ListSavedGames( saves );
	text = "quen: " + saves.Size() + " saves";
	for ( i = 0; i < saves.Size(); i += 1 )
	{
		LogChannel( 'Quen', saves[i].filename );
		// ponytail: the first 20 on screen; add a filter argument if picking from the screen matters.
		if ( i < 20 )
		{
			text += "<br>" + saves[i].filename;
		}
	}
	QUEN_Say( text );
}

// Exec functions can't be called from scripts, so the bodies live in the plain functions above.
exec function quenload( filename : string )
{
	QUEN_LoadSave( filename );
}

exec function quenlast()
{
	QUEN_LoadLast();
}

exec function quensaves()
{
	QUEN_ListSaves();
}

// ---- the command channel ----

// s split at its first space: all of s and "" without one.
function QUENRPC_Split(s : string, out head : string, out rest : string)
{
	if (!StrSplitFirst(s, " ", head, rest))
	{
		head = s;
		rest = "";
	}
}

// Every space-separated number of args, in order.
function QUENRPC_Numbers(args : string, out n : array<float>)
{
	var head, rest, left : string;

	n.Clear();
	left = args;
	while (left != "")
	{
		QUENRPC_Split(left, head, rest);
		n.PushBack(StringToFloat(head));
		left = rest;
	}
}

// "<camera> x y z pitch yaw roll fov", 4 decimals.
function QUENRPC_Pose(camera : string, pos : Vector, rot : EulerAngles, fov : float) : string
{
	return camera + " " + FloatToStringPrec(pos.X, 4) + " " + FloatToStringPrec(pos.Y, 4) + " " + FloatToStringPrec(pos.Z, 4)
		+ " " + FloatToStringPrec(rot.Pitch, 4) + " " + FloatToStringPrec(rot.Yaw, 4) + " " + FloatToStringPrec(rot.Roll, 4)
		+ " " + FloatToStringPrec(fov, 4);
}

// Sets the photo-mode camera to n: x y z pitch yaw roll, then fov if given. The probe boot's calls, as vanilla places it
// (photomodeManager.ws EnablePhotomode): the set held 120 menu ticks with 0 m deviation. ponytail: no guard for a
// location-preset blend the user starts by hand (none fought the set in the probe boot); the menu's private m_cameraBlendInProgress
// would need the menu instance.
function QUENRPC_SetCam(n : array<float>)
{
	var pos : Vector;
	var rot : EulerAngles;
	var cam : CCustomCamera;

	pos.X = n[0];
	pos.Y = n[1];
	pos.Z = n[2];
	pos.W = 1.0;
	rot.Pitch = n[3];
	rot.Yaw = n[4];
	rot.Roll = n[5];
	cam = theGame.GetPhotomodeCamera();
	if (n.Size() > 6)
		cam.SetFov(n[6]);
	cam.SetCameraWorldPosition(pos);
	cam.SetCameraWorldRotation(rot);
	cam.UpdateWithoutInput(true);
}

// world.actors: the alive actors within radius of the player with tag (GetActorsInRange; '' any), or with class the
// gameplay entities of that class there (FindGameplayEntitiesInSphere's className, no query flag: actors or not, dead
// ones too), the player left out, nearest first (SortNodesByDistance), at most max: "<found>", then per entity
// "|x y z heading vx vy vz distance flags id|<display name>|<readable name>". The velocity is an actor's moving agent
// component's GetVelocity (flag 64 when it has one); flags 1 IsInCombat, 2 GetAttitude towards the player AIA_Hostile,
// 4 GetNPCType ENGT_Guard, 8 IsAtWork, 16 IsAPValid(GetActiveActionPoint), 32 IsInGameplayScene (1, 2 and 32 only for
// an actor, 4, 8 and 16 only for a CNewNPC), 128 the readable name cut, 256 not an actor; id GetGuidHash (from quen-w3
// 0.1.12, with tag, class and flag 256). A '|' in a name made '/'; the display name cut at 80 characters, the readable
// name (GetReadableName: entity.ws, the text after the last "::" of ToString) at 255 (from quen-w3 0.1.11; 0.1.10 cut
// it at 80 without saying so).
function QUENRPC_Actors(max : int, radius : float, tag : name, cls : name) : string
{
	var actors : array<CActor>;
	var entities : array<CGameplayEntity>;
	var nodes : array<CNode>;
	var thing : CGameplayEntity;
	var actor : CActor;
	var npc : CNewNPC;
	var mac : CMovingAgentComponent;
	var pos, vel, at : Vector;
	var i, flags : int;
	var text, entity : string;

	at = thePlayer.GetWorldPosition();
	if (cls == '')
	{
		actors = GetActorsInRange(thePlayer, radius, 0, tag, true);
		for (i = 0; i < actors.Size(); i += 1)
		{
			if (actors[i] != thePlayer)
				nodes.PushBack(actors[i]);
		}
	}
	else
	{
		FindGameplayEntitiesInSphere(entities, at, radius, 1000000, tag, 0, NULL, cls);
		for (i = 0; i < entities.Size(); i += 1)
		{
			if (entities[i] != thePlayer)
				nodes.PushBack(entities[i]);
		}
	}
	SortNodesByDistance(at, nodes);
	text = IntToString(nodes.Size());
	for (i = 0; i < Min(max, nodes.Size()); i += 1)
	{
		thing = (CGameplayEntity)nodes[i];
		actor = (CActor)thing;
		npc = (CNewNPC)actor;
		pos = thing.GetWorldPosition();
		vel = Vector(0, 0, 0);
		flags = 0;
		if (!actor)
			flags += 256;
		if (actor)
			mac = actor.GetMovingAgentComponent();
		else
			mac = NULL;
		if (mac)
		{
			vel = mac.GetVelocity();
			flags += 64;
		}
		if (actor && actor.IsInCombat())
			flags += 1;
		if (actor && actor.GetAttitude(thePlayer) == AIA_Hostile)
			flags += 2;
		if (npc && npc.GetNPCType() == ENGT_Guard)
			flags += 4;
		if (npc && npc.IsAtWork())
			flags += 8;
		if (npc && IsAPValid(npc.GetActiveActionPoint()))
			flags += 16;
		if (actor && actor.IsInGameplayScene())
			flags += 32;
		entity = StrReplaceAll(thing.GetReadableName(), "|", "/");
		if (StrLen(entity) > 255)
		{
			flags += 128;
			entity = StrLeft(entity, 255);
		}
		text += "|" + FloatToStringPrec(pos.X, 4) + " " + FloatToStringPrec(pos.Y, 4) + " " + FloatToStringPrec(pos.Z, 4)
			+ " " + FloatToStringPrec(thing.GetHeading(), 4) + " " + FloatToStringPrec(vel.X, 4) + " "
			+ FloatToStringPrec(vel.Y, 4) + " " + FloatToStringPrec(vel.Z, 4) + " " + FloatToStringPrec(VecDistance(at, pos), 4)
			+ " " + IntToString(flags) + " " + IntToString(thing.GetGuidHash()) + "|"
			+ StrLeft(StrReplaceAll(thing.GetDisplayName(), "|", "/"), 80) + "|" + entity;
	}
	return text;
}

// ---- scene reads (from quen-w3 0.1.13): read only, numbers 4 decimals, W3Quen.cpp turns them into JSON ----

// "x y z", 4 decimals.
function QUEN_V(v : Vector) : string
{
	return FloatToStringPrec(v.X, 4) + " " + FloatToStringPrec(v.Y, 4) + " " + FloatToStringPrec(v.Z, 4);
}

// The collision groups quen's rays and sweeps test: photo mode's own raycast list (photomodeMenu.ws
// InitCollisionGroups) without Phantom, Water and Debris, with Foliage. No Character: a ray to a body's point isn't
// stopped by that body, nor by anyone else.
function QUEN_Groups() : array<name>
{
	var g : array<name>;

	g.PushBack('Static');
	g.PushBack('Terrain');
	g.PushBack('Foliage');
	g.PushBack('RigidBody');
	g.PushBack('Dynamic');
	g.PushBack('Destructible');
	g.PushBack('Door');
	g.PushBack('Platforms');
	g.PushBack('Fence');
	g.PushBack('Boat');
	g.PushBack('BoatDocking');
	return g;
}

// scene.frame: the alive actors within radius of the rendered camera, or of `at` (FindGameplayEntitiesInSphere,
// FLAG_OnlyAliveActors, the player added if missing), with an id only the one whose GetGuidHash it is, nearest that
// centre first (SortNodesByDistance), at most max: "<found> <camera x y z> <pitch yaw roll> <fov>" (theCamera's), then per
// entity "|<flags> <id> <x> <y> <share> <distance> <x y z> <heading> <head x y z> <torso x y z>|<display name>|<readable
// name>" as W3Quen.cpp's FrameJson reads it. The box: with a head bone, the body's (from 0.14), from the feet to 0.15 m
// above the head bone and 0.3 m each way along the camera's horizontal axes; else CalcBoundingBox's, which holds every
// component (boot 190: 2 to 5 times the body). x, y: theCamera.WorldVectorToViewRatio of the box's centre (flag 1 when in
// front). share: the box's 8 corners projected, those in front of the camera only (flag 2 when all 8), their
// rectangle clipped to the frame (-1 to 1), its area over the frame's (4). distance: from the camera. head and torso:
// GetBoneWorldPositionByIndex of GetHeadBoneIndex and GetTorsoBoneIndex (flags 8, 16), actors only. The first `points`
// of head, torso and origin each get a StaticTrace from the camera over QUEN_Groups (flags 256, 512, 1024), clear when
// it hits nothing (32, 64, 128). Names as QUENRPC_Actors cuts them (2048: the readable name cut). 4096: the actor's
// IsInView() (actor.ws; vanilla never calls it, its meaning unverified).
function QUENRPC_Frame(n : array<float>, id : int) : string
{
	var entities : array<CGameplayEntity>;
	var nodes : array<CNode>;
	var corner : array<Vector>;
	var groups : array<name>;
	var thing : CGameplayEntity;
	var actor : CActor;
	var world : CWorld;
	var box : Box;
	var rot : EulerAngles;
	var cam, centre, pos, head, torso, mid, hit, normal, right, a, b : Vector;
	var x, y, minX, minY, maxX, maxY, share, top : float;
	var i, k, flags, front, bone, points : int;
	var text, entity : string;

	world = theGame.GetWorld();
	groups = QUEN_Groups();
	cam = theCamera.GetCameraPosition();
	rot = theCamera.GetCameraRotation();
	// The body box's half-widths, along the camera's horizontal right and forward.
	right = VecNormalize2D(theCamera.GetCameraRight());
	a = 0.3 * right;
	b = Vector(-a.Y, a.X, 0, 0);
	centre = cam;
	if (n[3] > 0.5)
		centre = Vector(n[4], n[5], n[6], 1.0);
	points = (int)n[2];
	FindGameplayEntitiesInSphere(entities, centre, n[0], 1000000, '', FLAG_OnlyAliveActors);
	if (thePlayer && !entities.Contains(thePlayer) && VecDistance(centre, thePlayer.GetWorldPosition()) <= n[0])
		entities.PushBack(thePlayer);
	for (i = 0; i < entities.Size(); i += 1)
	{
		if (n[7] < 0.5 || entities[i].GetGuidHash() == id)
			nodes.PushBack(entities[i]);
	}
	SortNodesByDistance(centre, nodes);
	text = IntToString(nodes.Size()) + " " + QUEN_V(cam) + " " + FloatToStringPrec(rot.Pitch, 4) + " "
		+ FloatToStringPrec(rot.Yaw, 4) + " " + FloatToStringPrec(rot.Roll, 4) + " " + FloatToStringPrec(theCamera.GetFov(), 4);
	for (i = 0; i < Min((int)n[1], nodes.Size()); i += 1)
	{
		thing = (CGameplayEntity)nodes[i];
		actor = (CActor)thing;
		pos = thing.GetWorldPosition();
		flags = 0;
		if (thing == thePlayer)
			flags += 4;
		head = Vector(0, 0, 0, 1.0);
		torso = Vector(0, 0, 0, 1.0);
		if (actor)
		{
			bone = actor.GetHeadBoneIndex();
			if (bone != -1)
			{
				head = actor.GetBoneWorldPositionByIndex(bone);
				flags += 8;
			}
			bone = actor.GetTorsoBoneIndex();
			if (bone != -1)
			{
				torso = actor.GetBoneWorldPositionByIndex(bone);
				flags += 16;
			}
			if (actor.IsInView())
				flags += 4096;
		}
		corner.Clear();
		if (flags % 16 >= 8)
		{
			// The body: 0.3 m fitted Geralt's pixel box in gameplay and in photo mode within 3 % (boot 190's captures).
			top = head.Z + 0.15;
			corner.PushBack(Vector(pos.X - a.X - b.X, pos.Y - a.Y - b.Y, pos.Z, 1.0));
			corner.PushBack(Vector(pos.X + a.X - b.X, pos.Y + a.Y - b.Y, pos.Z, 1.0));
			corner.PushBack(Vector(pos.X - a.X + b.X, pos.Y - a.Y + b.Y, pos.Z, 1.0));
			corner.PushBack(Vector(pos.X + a.X + b.X, pos.Y + a.Y + b.Y, pos.Z, 1.0));
			corner.PushBack(Vector(pos.X - a.X - b.X, pos.Y - a.Y - b.Y, top, 1.0));
			corner.PushBack(Vector(pos.X + a.X - b.X, pos.Y + a.Y - b.Y, top, 1.0));
			corner.PushBack(Vector(pos.X - a.X + b.X, pos.Y - a.Y + b.Y, top, 1.0));
			corner.PushBack(Vector(pos.X + a.X + b.X, pos.Y + a.Y + b.Y, top, 1.0));
			mid = Vector(pos.X, pos.Y, (pos.Z + top) / 2, 1.0);
		}
		else
		{
			thing.CalcBoundingBox(box);
			corner.PushBack(Vector(box.Min.X, box.Min.Y, box.Min.Z, 1.0));
			corner.PushBack(Vector(box.Max.X, box.Min.Y, box.Min.Z, 1.0));
			corner.PushBack(Vector(box.Min.X, box.Max.Y, box.Min.Z, 1.0));
			corner.PushBack(Vector(box.Max.X, box.Max.Y, box.Min.Z, 1.0));
			corner.PushBack(Vector(box.Min.X, box.Min.Y, box.Max.Z, 1.0));
			corner.PushBack(Vector(box.Max.X, box.Min.Y, box.Max.Z, 1.0));
			corner.PushBack(Vector(box.Min.X, box.Max.Y, box.Max.Z, 1.0));
			corner.PushBack(Vector(box.Max.X, box.Max.Y, box.Max.Z, 1.0));
			mid = Vector((box.Min.X + box.Max.X) / 2, (box.Min.Y + box.Max.Y) / 2, (box.Min.Z + box.Max.Z) / 2, 1.0);
		}
		front = 0;
		minX = 1000000;
		minY = 1000000;
		maxX = -1000000;
		maxY = -1000000;
		for (k = 0; k < 8; k += 1)
		{
			if (theCamera.WorldVectorToViewRatio(corner[k], x, y))
			{
				front += 1;
				minX = MinF(minX, x);
				maxX = MaxF(maxX, x);
				minY = MinF(minY, y);
				maxY = MaxF(maxY, y);
			}
		}
		share = 0;
		if (front > 0)
			share = MaxF(0, MinF(maxX, 1) - MaxF(minX, -1)) * MaxF(0, MinF(maxY, 1) - MaxF(minY, -1)) / 4;
		if (front == 8)
			flags += 2;
		x = 0;
		y = 0;
		if (theCamera.WorldVectorToViewRatio(mid, x, y))
			flags += 1;
		if (points > 0 && flags % 16 >= 8)
		{
			flags += 256;
			if (!world.StaticTrace(cam, head, hit, normal, groups))
				flags += 32;
		}
		if (points > 1 && flags % 32 >= 16)
		{
			flags += 512;
			if (!world.StaticTrace(cam, torso, hit, normal, groups))
				flags += 64;
		}
		if (points > 2)
		{
			flags += 1024;
			if (!world.StaticTrace(cam, pos, hit, normal, groups))
				flags += 128;
		}
		entity = StrReplaceAll(thing.GetReadableName(), "|", "/");
		if (StrLen(entity) > 255)
		{
			flags += 2048;
			entity = StrLeft(entity, 255);
		}
		text += "|" + IntToString(flags) + " " + IntToString(thing.GetGuidHash()) + " " + FloatToStringPrec(x, 4) + " "
			+ FloatToStringPrec(y, 4) + " " + FloatToStringPrec(share, 4) + " " + FloatToStringPrec(VecDistance(cam, pos), 4) + " "
			+ QUEN_V(pos) + " " + FloatToStringPrec(thing.GetHeading(), 4) + " " + QUEN_V(head) + " " + QUEN_V(torso) + "|"
			+ StrLeft(StrReplaceAll(thing.GetDisplayName(), "|", "/"), 80) + "|" + entity;
	}
	return text;
}

// scene.ground, n = range radius search, then x y z per point: the navmesh z within range of z (NavigationComputeZ, flag
// 1), a StaticTrace down from z + range to z - range over QUEN_Groups (flag 2: its hit and normal), GetWaterLevel and
// GetWaterDepth at the point, then from the navmesh z with radius above 0 NavigationCircleTest (flag 8, free 4) and with
// search above 0 NavigationFindSafeSpot with radius as its personal space (flag 16): "<n>", then per point "|<flags>
// <navmesh z> <hit x y z> <normal x y z> <water level> <water depth> <safe spot x y z>".
function QUENRPC_Ground(n : array<float>) : string
{
	var world : CWorld;
	var groups : array<name>;
	var p, a, b, on, hit, normal, safe : Vector;
	var navZ, level, depth : float;
	var i, flags : int;
	var text : string;

	world = theGame.GetWorld();
	groups = QUEN_Groups();
	text = IntToString((n.Size() - 3) / 3);
	for (i = 3; i + 2 < n.Size(); i += 3)
	{
		p = Vector(n[i], n[i + 1], n[i + 2], 1.0);
		flags = 0;
		navZ = 0;
		if (world.NavigationComputeZ(p, p.Z - n[0], p.Z + n[0], navZ))
			flags += 1;
		a = p;
		a.Z += n[0];
		b = p;
		b.Z -= n[0];
		hit = Vector(0, 0, 0, 1.0);
		normal = Vector(0, 0, 0, 0);
		if (world.StaticTrace(a, b, hit, normal, groups))
			flags += 2;
		level = world.GetWaterLevel(p);
		depth = world.GetWaterDepth(p);
		on = p;
		if (flags % 2 == 1)
			on.Z = navZ;
		if (n[1] > 0 && flags % 2 == 1)
		{
			flags += 8;
			if (world.NavigationCircleTest(on, n[1]))
				flags += 4;
		}
		safe = Vector(0, 0, 0, 1.0);
		if (n[2] > 0 && world.NavigationFindSafeSpot(on, n[1], n[2], safe))
			flags += 16;
		text += "|" + IntToString(flags) + " " + FloatToStringPrec(navZ, 4) + " " + QUEN_V(hit) + " " + QUEN_V(normal) + " "
			+ FloatToStringPrec(level, 4) + " " + FloatToStringPrec(depth, 4) + " " + QUEN_V(safe);
	}
	return text;
}

// scene.ray, n = radius then ax ay az bx by bz per segment: a StaticTrace from a to b over QUEN_Groups, or with radius
// above 0 a SweepTest of that radius: "<n>", then per segment "|<flags> <hit x y z> <normal x y z> <distance from a>",
// flag 1 a hit.
function QUENRPC_Ray(n : array<float>) : string
{
	var world : CWorld;
	var groups : array<name>;
	var a, b, hit, normal : Vector;
	var i : int;
	var hitOk : bool;
	var text : string;

	world = theGame.GetWorld();
	groups = QUEN_Groups();
	text = IntToString((n.Size() - 1) / 6);
	for (i = 1; i + 5 < n.Size(); i += 6)
	{
		a = Vector(n[i], n[i + 1], n[i + 2], 1.0);
		b = Vector(n[i + 3], n[i + 4], n[i + 5], 1.0);
		hit = Vector(0, 0, 0, 1.0);
		normal = Vector(0, 0, 0, 0);
		if (n[0] > 0)
			hitOk = world.SweepTest(a, b, n[0], hit, normal, groups);
		else
			hitOk = world.StaticTrace(a, b, hit, normal, groups);
		if (hitOk)
			text += "|1 ";
		else
			text += "|0 ";
		text += QUEN_V(hit) + " " + QUEN_V(normal) + " " + FloatToStringPrec(VecDistance(a, hit), 4);
	}
	return text;
}

// nav.line, n = width then ax ay az bx by bz per segment: NavigationLineTest (flag 1), NavigationClearLineInDirection
// (flag 2: it returned true; its point either way) and NavigationClosestObstacleToLine, each with width as its radius:
// "<n>", then per segment "|<flags> <point x y z> <obstacle distance>".
function QUENRPC_Nav(n : array<float>) : string
{
	var world : CWorld;
	var a, b, reach, onLine, onGeometry : Vector;
	var i, flags : int;
	var edge : float;
	var text : string;

	world = theGame.GetWorld();
	text = IntToString((n.Size() - 1) / 6);
	for (i = 1; i + 5 < n.Size(); i += 6)
	{
		a = Vector(n[i], n[i + 1], n[i + 2], 1.0);
		b = Vector(n[i + 3], n[i + 4], n[i + 5], 1.0);
		flags = 0;
		if (world.NavigationLineTest(a, b, n[0]))
			flags += 1;
		reach = a;
		if (world.NavigationClearLineInDirection(a, b, n[0], reach))
			flags += 2;
		edge = world.NavigationClosestObstacleToLine(a, b, n[0], onLine, onGeometry);
		text += "|" + IntToString(flags) + " " + QUEN_V(reach) + " " + FloatToStringPrec(edge, 4);
	}
	return text;
}

// The free camera's state (quen-w3 0.1.17): "<on 0|1> <paused 0|1> <free camera x y z> <rendered camera x y z pitch yaw
// roll> <player x y z heading> <radius> <near camera> <near player>", as the free-camera probe's qfcstatus read it (boot
// 237): IsFreeCameraEnabled, GetFreeCameraPosition (it keeps its last pose while off), theCamera (the rendered view,
// equal to the free camera while it is on; nothing reads the free camera's own rotation), IsPaused, and the alive
// actors within radius of the rendered camera and of the player, the player among them when in range
// (FindGameplayEntitiesInSphere with FLAG_OnlyAliveActors).
function QUENRPC_FreeCam(radius : float) : string
{
	var ents : array<CGameplayEntity>;
	var cam, p : Vector;
	var r : EulerAngles;
	var text : string;

	cam = theCamera.GetCameraPosition();
	r = theCamera.GetCameraRotation();
	p = thePlayer.GetWorldPosition();
	text = "0";
	if (theGame.IsFreeCameraEnabled())
		text = "1";
	if (theGame.IsPaused())
		text += " 1";
	else
		text += " 0";
	text += " " + QUEN_V(theGame.GetFreeCameraPosition()) + " " + QUEN_V(cam) + " " + FloatToStringPrec(r.Pitch, 4) + " "
		+ FloatToStringPrec(r.Yaw, 4) + " " + FloatToStringPrec(r.Roll, 4) + " " + QUEN_V(p) + " "
		+ FloatToStringPrec(thePlayer.GetHeading(), 4) + " " + FloatToStringPrec(radius, 4);
	FindGameplayEntitiesInSphere(ents, cam, radius, 1000, '', FLAG_OnlyAliveActors);
	text += " " + IntToString(ents.Size());
	ents.Clear();
	FindGameplayEntitiesInSphere(ents, p, radius, 1000, '', FLAG_OnlyAliveActors);
	return text + " " + IntToString(ents.Size());
}

// The photo-mode camera's own pose: the read-back of a set.
function QUENRPC_PhotoPose() : string
{
	var cam : CCustomCamera;

	cam = theGame.GetPhotomodeCamera();
	return QUENRPC_Pose("photomode", cam.GetCameraWorldPosition(), cam.GetCameraWorldRotation(), cam.GetFov());
}

// A method, so it can set the camera path fields: a global function sees theGame as const.
@addMethod(CR4Game) function QUENRPC_Poll()
{
	var line, head, rest, verb, args, text : string;
	var handle, i, slots, manual, count : int;
	var photo : bool;
	var n : array<float>;
	var pos : Vector;
	var rot : EulerAngles;
	var saves : array<SSavegameInfo>;
	var weathers : array<name>;
	var paths : array<string>;
	var t : GameTime;
	var gui : CR4GuiManager;
	var popup : CR4OverlayPopup;
	var fx : CR4PhotomodeEffects;
	var item, cls : name;
	var ids : array<SItemUniqueId>;
	var before, after : int;
	var equipped : bool;
	var witcher : W3PlayerWitcher;

	line = QUEN_Next();
	if (line == "")
		return;
	QUENRPC_Split(line, head, rest);
	handle = StringToInt(head);
	QUENRPC_Split(rest, verb, args);
	photo = theGame.GetPhotomodeEnabled();
	// The effects are on only while photo mode's menu is open (its OnMenuShown and OnClosingMenu).
	if (StrBeginsWith(verb, "photo.") && verb != "photo.mode" && (!photo || !quenPhotoMenu))
	{
		QUEN_Reply(handle, false, verb + " needs photo mode with its menu open (photo.mode on opens it)", "");
		return;
	}
	fx = GetPhotomodeEffects();

	switch (verb)
	{
		case "photo.mode":
			// On: EnablePhotomode, as photo mode's key calls it. Off: the menu's own exit, OnConfirmExit (it clears what the
			// user spawned, then DisablePhotomode, which restores the weather), never its "Exit Photo Mode?" dialog: escape
			// shows that only when HaveChangesBeenMade, else exits at once (photomodeMenu.ws OnTryClosingMenu).
			if (args == "1")
			{
				if (photo)
					QUEN_Reply(handle, true, "photo mode is already on", "");
				else if (!thePlayer)
					QUEN_Reply(handle, false, "no player (not in a game session)", "");
				else
				{
					text = thePlayer.GetPhotomodeManager().QUEN_Enable();
					if (text != "")
						QUEN_Reply(handle, false, "photo mode doesn't open now (vanilla's own check): " + text, "");
					else if (!GetPhotomodeEnabled())
						QUEN_Reply(handle, false, "EnablePhotomode returned without photo mode", "");
					else
						QUEN_Reply(handle, true, "photo mode on; its menu opens on a later tick", "");
				}
				return;
			}
			if (!photo)
			{
				QUEN_Reply(handle, true, "photo mode is already off", "");
				return;
			}
			if (quenPhotoMenu)
				quenPhotoMenu.OnConfirmExit();
			else
				thePlayer.GetPhotomodeManager().DisablePhotomode();
			QUEN_Reply(handle, true, "leaving photo mode by its menu's own exit, no dialog", "");
			return;

		case "photo.get":
			QUEN_Reply(handle, true, "photo mode's time, grading, weather and env", QUENRPC_PhotoState());
			return;

		case "photo.weather":
			// The menu's own call (PM_Weather); only names the area's list holds, as for weather.
			GetWeatherNames(weathers);
			text = "";
			for (i = 0; i < weathers.Size(); i += 1)
			{
				if (NameToString(weathers[i]) == args)
				{
					fx.SetWeather(args);
					QUEN_Reply(handle, true, "photo mode's weather set; data is the read-back", QUENRPC_PhotoState());
					return;
				}
				text += " " + NameToString(weathers[i]);
			}
			QUEN_Reply(handle, false, "unknown weather " + args + "; this area has:" + text, "");
			return;

		case "photo.env":
			// The menu's own call (PM_EnvDef), with a path of GetAllEnvironmentDefinitions, any case, sent as it lists it.
			GetAllEnvironmentDefinitions(paths);
			for (i = 0; i < paths.Size(); i += 1)
			{
				if (StrLower(paths[i]) == StrLower(args))
				{
					fx.SetEnvDefUsingPath(paths[i]);
					QUEN_Reply(handle, true, "photo mode's env set; data is the read-back", QUENRPC_PhotoState());
					return;
				}
			}
			QUEN_Reply(handle, false, "no environment definition " + args + " (photo.list envs lists them)", "");
			return;

		case "photo.time":
			// The menu's own calls (PM_TimeOfDay): its slider passes minutes of the day; then the city lights.
			fx.SetTimeOfDay(StringToFloat(args));
			GetCityLightManager().SetUpdateEnabled(true);
			GetCityLightManager().ForceUpdate();
			QUEN_Reply(handle, true, "photo mode's time set; data is the read-back", QUENRPC_PhotoState());
			return;

		case "photo.grade":
			// 8 pairs "<given 0|1> <value>" in QUEN_Grade's order, from the ASI; the menu's own setters.
			QUENRPC_Numbers(args, n);
			for (i = 0; i < 8; i += 1)
			{
				if (n[2 * i] > 0.5)
					QUEN_SetGrade(fx, i, n[2 * i + 1]);
			}
			QUEN_Reply(handle, true, "photo mode's grading set; data is the read-back", QUENRPC_PhotoState());
			return;

		case "photo.reset":
			// weather: RestoreWeather, the menu's "none" (and DisablePhotomode's); env: SetEnvDefNull, the menu's "none";
			// grading: the sliders back to their values as the menu opened.
			if (args == "weather")
				fx.RestoreWeather();
			else if (args == "env")
				fx.SetEnvDefNull();
			else
			{
				for (i = 0; i < 8; i += 1)
					QUEN_SetGrade(fx, i, quenGradeOpen[i]);
			}
			QUEN_Reply(handle, true, "photo mode's " + args + " reset; data is the read-back", QUENRPC_PhotoState());
			return;

		case "photo.tick":
			// 2 pairs "<given 0|1> <seconds>": the menu's own calls (DoFakeTick passes 0.00001 s, WarmupClothsSimulation 1 s).
			QUENRPC_Numbers(args, n);
			if (n[0] > 0.5)
				fx.SetPhotomodeTickTime(n[1]);
			if (n[2] > 0.5)
				fx.SetPhotomodeClothsSimulationTime(n[3]);
			QUEN_Reply(handle, true, "called; neither has a getter to read back", "");
			return;

		case "photo.list":
			// "weathers" or "envs", then a filter (case-insensitive, may hold spaces): GetWeatherNames, the area's list, or
			// GetAllEnvironmentDefinitions, every env path; the menu shows fewer of either.
			QUENRPC_Split(args, head, rest);
			if (head == "weathers")
			{
				GetWeatherNames(weathers);
				for (i = 0; i < weathers.Size(); i += 1)
					paths.PushBack(NameToString(weathers[i]));
			}
			else
				GetAllEnvironmentDefinitions(paths);
			text = "";
			count = 0;
			for (i = 0; i < paths.Size(); i += 1)
			{
				if (rest != "" && StrFindFirst(StrLower(paths[i]), StrLower(rest)) < 0)
					continue;
				if (count > 0)
					text += "|";
				text += paths[i];
				count += 1;
			}
			// ponytail: a cap below the ASI's 64 KiB reply, so the reason says how many matched.
			if (StrLen(text) > 60000)
				QUEN_Reply(handle, false, IntToString(count) + " of " + IntToString(paths.Size()) + " " + head
					+ " match, over 60000 characters: narrow the filter", "");
			else
				QUEN_Reply(handle, true, IntToString(count) + " of " + IntToString(paths.Size()) + " " + head, text);
			return;

		case "load":
			if (QUEN_LoadSave(args))
				QUEN_Reply(handle, true, "loading the save matching " + args, "");
			else
				QUEN_Reply(handle, false, "not exactly one save matches " + args + " (listsaves lists them)", "");
			return;

		case "loadlast":
			QUEN_LoadLast();
			QUEN_Reply(handle, true, "loading the last save", "");
			return;

		case "listsaves":
			theGame.ListSavedGames(saves);
			text = "";
			for (i = 0; i < saves.Size(); i += 1)
			{
				if (i > 0)
					text += "|";
				text += saves[i].filename;
			}
			QUEN_Reply(handle, true, IntToString(saves.Size()) + " saves", text);
			return;

		case "cam.get":
			// As the probe boot read it: the photo camera's own pose; theCamera (the rendered view) agrees one frame later.
			if (photo)
			{
				QUEN_Reply(handle, true, "", QUENRPC_PhotoPose());
			}
			else
			{
				QUEN_Reply(handle, true, "", QUENRPC_Pose("game", theCamera.GetCameraPosition(), theCamera.GetCameraRotation(), theCamera.GetFov()));
			}
			return;

		case "cam.set":
			QUENRPC_Numbers(args, n);
			if (!photo)
			{
				QUEN_Reply(handle, false, "cam.set needs photo mode: it sets the photo-mode camera", "");
				return;
			}
			quenCamPath = 0;  // a set stops a path
			QUENRPC_SetCam(n);
			QUEN_Reply(handle, true, "set; data is the read-back", QUENRPC_PhotoPose());
			return;

		case "cam.path":
			// "<step 0|1> <frames>": W3Quen.asi checked the path and holds it. A new path replaces a loaded one; cam.set,
			// cam.stop and leaving photo mode stop it. The first pose is set now, and real time runs from it.
			QUENRPC_Split(args, head, rest);
			quenCamPath = 0;
			if (!photo)
			{
				QUEN_Reply(handle, false, "cam.path needs photo mode: it moves the photo-mode camera", "");
				return;
			}
			quenCamFrame = 0;
			quenCamFrames = StringToInt(rest);
			quenCamPath = 1;
			text = "real-time path playing";
			if (head == "1")
			{
				quenCamPath = 2;
				text = "frame-step path loaded, " + rest + " frames; frame 0 set";
			}
			if (!QUEN_CamTick())
			{
				QUEN_Reply(handle, false, "W3Quen.asi gave no pose for the path", "");
				return;
			}
			QUEN_Reply(handle, true, text + "; data is the read-back", QUENRPC_PhotoPose());
			return;

		case "cam.frame":
			if (!photo || quenCamPath != 2)
			{
				QUEN_Reply(handle, false, "no frame-step path is loaded (cam.path with mode step, in photo mode)", "");
				return;
			}
			i = StringToInt(args);
			if (i >= quenCamFrames)
			{
				QUEN_Reply(handle, false, "frame " + args + " is past the path's last, " + IntToString(quenCamFrames - 1), "");
				return;
			}
			quenCamFrame = i;
			QUEN_CamTick();
			QUEN_Reply(handle, true, "frame " + args + " set; data is the read-back", QUENRPC_PhotoPose());
			return;

		case "cam.stop":
			text = "no path was loaded";
			if (quenCamPath != 0)
				text = "path stopped; the camera stays where it is";
			quenCamPath = 0;
			QUEN_Reply(handle, true, text, "");
			return;

		case "teleport":
			QUENRPC_Numbers(args, n);
			if (!thePlayer)
			{
				QUEN_Reply(handle, false, "no player (not in a game session)", "");
				return;
			}
			pos.X = n[0];
			pos.Y = n[1];
			pos.Z = n[2];
			pos.W = 1.0;
			rot.Pitch = n[3];
			rot.Yaw = n[4];
			rot.Roll = n[5];
			thePlayer.TeleportWithRotation(pos, rot);
			QUEN_Reply(handle, true, "teleported the player (z snaps to the ground; the pose reads back from the next tick)", "");
			return;

		case "settime":
			// As vanilla exec function settime (engine\gameTime.ws). In photo mode it moved the sun at once (2026-10-19).
			QUENRPC_Split(args, head, rest);
			theGame.SetGameTime(GameTimeCreate(StringToInt(head), StringToInt(rest), 0, 0), true);
			QUEN_Reply(handle, true, "game time set to day " + head + " hour " + rest, "");
			return;

		case "weather":
			// As vanilla exec function changeweather (game\temp.ws), 1 s blend; only names the area's list holds.
			GetWeatherNames(weathers);
			text = "";
			for (i = 0; i < weathers.Size(); i += 1)
			{
				if (NameToString(weathers[i]) == args)
				{
					if (RequestWeatherChangeTo(weathers[i], 1.0, false))
						QUEN_Reply(handle, true, "weather changing to " + args, "");
					else
						QUEN_Reply(handle, false, "the game refused weather " + args, "");
					return;
				}
				text += " " + NameToString(weathers[i]);
			}
			QUEN_Reply(handle, false, "unknown weather " + args + "; this area has:" + text, "");
			return;

		case "save":
			// The menu's "Empty save slot" (ingameMenu.ws OnSaveGameCalled, igmUtilities.ws): a new manual save, slot -1,
			// never an overwrite; the game names the file, and quen checks gamesaves before and after. Plain gameplay
			// only, as QUEN_PollState's flags tell it, and the game's own save lock.
			text = "";
			if (!thePlayer || !theGame.AreSavesInitialized())
				text = "no game to save (the main menu, or the game's saves not ready)";
			else if (photo)
				text = "photo mode: a save needs plain gameplay";
			else if (IsCurrentlyPlayingNonGameplayScene() || thePlayer.IsInNonGameplayCutscene() || IsBlackscreenOrFading()
				|| IsLoadingScreenVideoPlaying() || IsDialogOrCutscenePlaying())
				text = "a scene, cutscene, fade or dialogue: a save needs plain gameplay";
			else if (theGame.AreSavesLocked())
				text = "the game locks saves now (combat, a menu and the like): its own rule, as its menu shows";
			else
			{
				// The menu shows no empty slot once the manual saves fill the slot count (-1: no limit).
				slots = theGame.GetNumSaveSlots(SGT_Manual);
				theGame.ListSavedGames(saves, SGT_Manual);
				manual = 0;
				for (i = 0; i < saves.Size(); i += 1)
				{
					if (saves[i].slotType == SGT_Manual)
						manual += 1;
				}
				if (slots != -1 && manual >= slots)
					text = "no empty manual save slot: " + IntToString(manual) + " of " + IntToString(slots);
			}
			if (text != "")
			{
				QUEN_Reply(handle, false, text, "");
				return;
			}
			theGame.SaveGame(SGT_Manual, -1);
			QUEN_Reply(handle, true, "save requested", "");
			return;

		case "world.get":
			// Read only. The hour is GameTimeHours, which vanilla GetDayPart reads as the hour of the day (0-23).
			if (!thePlayer)
			{
				QUEN_Reply(handle, false, "no player (not in a game session)", "");
				return;
			}
			pos = thePlayer.GetWorldPosition();
			t = theGame.GetGameTime();
			QUEN_Reply(handle, true, "the player's position and heading, the game time and the weather",
				FloatToStringPrec(pos.X, 4) + " " + FloatToStringPrec(pos.Y, 4) + " " + FloatToStringPrec(pos.Z, 4) + " "
				+ FloatToStringPrec(thePlayer.GetHeading(), 4) + " " + IntToString(GameTimeDays(t)) + " "
				+ IntToString(GameTimeHours(t)) + " " + QUENRPC_Env() + " " + NameToString(GetWeatherConditionName()));
			return;

		case "world.actors":
			// Read only: "<max> <radius>", then "<tag> <class>" ('-' for none) from quen-w3 0.1.12, which the ASI checked.
			if (!thePlayer)
			{
				QUEN_Reply(handle, false, "no player (not in a game session)", "");
				return;
			}
			QUENRPC_Split(args, head, rest);
			count = StringToInt(head);
			QUENRPC_Split(rest, head, rest);
			QUENRPC_Split(rest, text, args);
			item = '';
			if (text != "" && text != "-")
				item = WNB_StringToName(text);
			cls = '';
			if (args != "" && args != "-")
				cls = WNB_StringToName(args);
			QUEN_Reply(handle, true, "the actors near the player, nearest first",
				QUENRPC_Actors(count, StringToFloat(head), item, cls));
			return;

		case "scene.frame":
		case "scene.ground":
		case "scene.ray":
		case "nav.line":
			// Read only, from quen-w3 0.1.13; the ASI checked the numbers' count and range. scene.frame's id is its last
			// number, read whole (a float holds no 32-bit int).
			if (!thePlayer)
			{
				QUEN_Reply(handle, false, "no player (not in a game session)", "");
				return;
			}
			QUENRPC_Numbers(args, n);
			if (verb == "scene.frame")
				text = QUENRPC_Frame(n, StringToInt(StrAfterLast(args, " ")));
			else if (verb == "scene.ground")
				text = QUENRPC_Ground(n);
			else if (verb == "scene.ray")
				text = QUENRPC_Ray(n);
			else
				text = QUENRPC_Nav(n);
			QUEN_Reply(handle, true, verb + " read", text);
			return;

		case "cam.noclip":
			// 1: no-clip on until photo mode's menu closes (QUEN_NoclipBack); 0: back to the menu's setting now.
			if (!photo)
			{
				QUEN_Reply(handle, false, "cam.noclip needs photo mode: it sets the photo-mode camera", "");
				return;
			}
			if (args == "1")
			{
				GetPhotomodeCamera().SetNoclip(true);
				quenNoclip = true;
				QUEN_Reply(handle, true, "no-clip on until photo mode closes; its menu's Noclip doesn't show it", "");
				return;
			}
			QUEN_NoclipBack();
			QUEN_Reply(handle, true, "no-clip as photo mode's menu has it", "");
			return;

		case "notice":
			// "<duration_ms> <text>", which the ASI checked: 1 to 3600000 ms (8000 from a quen before 0.1.15), 1 to 160
			// bytes, no control character. From 0.1.15, the same text still up is kept as it is, so quen's re-send every
			// 10 s doesn't take it down and back up. From 0.1.16 a lasting line goes on quen's HUD module where the HUD
			// draws it, and on the notification elsewhere (QUEN_Show); a game state change moves it (QUEN_PollState).
			QUENRPC_Split(args, head, text);
			line = QUEN_Show(text, StringToFloat(head));
			if (line == "")
			{
				QUEN_Reply(handle, false, "no overlay popup to show a notice on (a load, the main menu)", "");
				return;
			}
			quenLine = "";
			if (StringToFloat(head) >= 60000)
			{
				quenLine = text;
				quenLineMs = StringToFloat(head);
			}
			QUEN_Reply(handle, true, line, "");
			return;

		case "notice.hide":
			// From 0.1.15: ClearNotificationsQueue, so no queued notification follows into quen's capture: the 0.3 s fade
			// of hideNotification (OverlayPopupMenu.as), measured 0.312 s in the probe boot. No overlay popup: no
			// notification to hide. From 0.1.16 also quen's HUD module, within the call, and the line forgotten, so no
			// game state change brings it back.
			quenLine = "";
			QUEN_HudHide();
			gui = theGame.GetGuiManager();
			if (gui)
				popup = (CR4OverlayPopup)gui.GetPopup('OverlayPopup');
			if (!popup)
			{
				QUEN_Reply(handle, true, "no notice to hide (no overlay popup)", "");
				return;
			}
			popup.ClearNotificationsQueue();
			QUEN_Reply(handle, true, "notice hidden", "");
			return;

		case "sound":
			// From 0.1.15: a sound event by its name (1 to 64 letters, digits and _, the ASI checked), as the GUI's own
			// sounds play (theSound.SoundEvent("gui_global_denied")). An unknown name plays nothing, unseen here.
			theSound.SoundEvent(args);
			QUEN_Reply(handle, true, "sound played", "");
			return;

		case "items.add":
			// As vanilla exec function additem (game\temp.ws), the name made by the Native Bridge's WNB_StringToName (the
			// console needs quotes for a name with spaces). AddAnItem gives back an invalid id for a name no item has, so
			// the count before and after tells whether anything was added. The equip as additem's, read back as
			// W3PlayerWitcher.IsItemEquipped.
			if (!thePlayer)
			{
				QUEN_Reply(handle, false, "no player (not in a game session)", "");
				return;
			}
			QUENRPC_Split(args, head, rest);
			count = StringToInt(head);
			QUENRPC_Split(rest, head, text);
			item = WNB_StringToName(text);
			before = thePlayer.inv.GetItemQuantityByName(item);
			ids = thePlayer.inv.AddAnItem(item, count);
			after = thePlayer.inv.GetItemQuantityByName(item);
			equipped = false;
			if (head == "1" && ids.Size() > 0 && thePlayer.inv.IsIdValid(ids[0]))
			{
				thePlayer.EquipItem(ids[0]);
				witcher = GetWitcherPlayer();
				if (witcher)
					equipped = witcher.IsItemEquipped(ids[0]);
			}
			rest = IntToString(before) + " " + IntToString(after) + " 0";
			if (equipped)
				rest = IntToString(before) + " " + IntToString(after) + " 1";
			if (after > before)
				QUEN_Reply(handle, true, "added " + IntToString(after - before) + " of " + text, rest);
			else
				QUEN_Reply(handle, false, "nothing added: no item is named " + text + " (the item's own name, as additem takes it)", rest);
			return;

		case "freecam":
		case "freecam.set":
		case "freecam.get":
			// From quen-w3 0.1.17: the game's own free camera, CR4Game's developer natives (engine\game.ws), with the
			// world running (the free-camera probe, boot 237). freecam: EnableFreeCamera, "1" or "0"; freecam.set:
			// MoveFreeCamera to "x y z pitch yaw roll", only while it is on; freecam.get: "<radius>", nothing changed. Each
			// replies with the state (QUENRPC_FreeCam). Photo mode has its own camera: freecam on and freecam.set are
			// refused there.
			if (!thePlayer)
			{
				QUEN_Reply(handle, false, "no player (not in a game session)", "");
				return;
			}
			if (photo && (args == "1" || verb == "freecam.set"))
			{
				QUEN_Reply(handle, false, "photo mode has its own camera (cam.set): the free camera is for gameplay", "");
				return;
			}
			text = "the free camera's state";
			if (verb == "freecam")
			{
				theGame.EnableFreeCamera(args == "1");
				text = "EnableFreeCamera(" + args + ") called";
			}
			if (verb == "freecam.set")
			{
				if (!theGame.IsFreeCameraEnabled())
				{
					QUEN_Reply(handle, false, "the free camera is off: freecam on first", "");
					return;
				}
				QUENRPC_Numbers(args, n);
				pos.X = n[0];
				pos.Y = n[1];
				pos.Z = n[2];
				pos.W = 1.0;
				rot.Pitch = n[3];
				rot.Yaw = n[4];
				rot.Roll = n[5];
				theGame.MoveFreeCamera(pos, rot);
				text = "MoveFreeCamera called";
			}
			if (verb == "freecam.get")
				QUEN_Reply(handle, true, text, QUENRPC_FreeCam(StringToFloat(args)));
			else
				QUEN_Reply(handle, true, text + "; data is the state in the same tick", QUENRPC_FreeCam(30));
			return;

		case "pause":
			// theGame.Pause under quen's own reason, not SetTimeScale(0): Unpause("quen") lifts only ours (menus and other
			// reasons keep theirs), IsPausedForReason reads it back exactly, and there is no priority stack to lose to.
			// SetTimeScale also has a dontSave flag, so a scale can go into a save. Both froze the world in the probe boot,
			// and the tick that polls this channel kept running under both. Idempotent: on twice, off once.
			if (args == "1" && !theGame.IsPausedForReason("quen"))
				theGame.Pause("quen");
			if (args == "0" && theGame.IsPausedForReason("quen"))
				theGame.Unpause("quen");
			text = "quen's pause is off";
			if (theGame.IsPausedForReason("quen"))
				text = "quen's pause is on";
			if (theGame.IsPaused())
				text += "; the game is paused";
			else
				text += "; the game is running";
			QUEN_Reply(handle, true, text, "");
			return;
	}
	QUEN_Reply(handle, false, "modQuen doesn't know verb " + verb, "");
}
