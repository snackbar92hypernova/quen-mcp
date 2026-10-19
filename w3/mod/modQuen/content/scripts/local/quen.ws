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
//   changes nothing.
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
	theGame.QUEN_PhotoMenu(NULL);
	theGame.QUEN_PollState(true);
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
	}
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
	var fx : CR4PhotomodeEffects;

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

		case "notice":
			// quen's own text, which the ASI checked: 1 to 160 bytes, no control character.
			gui = theGame.GetGuiManager();
			if (!gui)
			{
				QUEN_Reply(handle, false, "no GUI manager to show a notice", "");
				return;
			}
			gui.ShowNotification(args, 8000);
			QUEN_Reply(handle, true, "notice shown", "");
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
