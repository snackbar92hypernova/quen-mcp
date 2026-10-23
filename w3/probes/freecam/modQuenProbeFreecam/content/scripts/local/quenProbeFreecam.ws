// modQuenProbeFreecam: a PROBE for a later quen free-camera tool, not for play (README.md beside this mod).
// Do the game's developer free-camera natives (CR4Game's EnableFreeCamera, MoveFreeCamera, GetFreeCameraPosition,
// IsFreeCameraEnabled) work in the retail 5.01, with the world running? Console execs turn it on and off, move it, and
// log one line each on LogChannel('QuenFC'). Nothing runs until typed. No resources, no wrapped vanilla method, no field.

function QuenFcPos(v : Vector) : string
{
	return FloatToStringPrec(v.X, 2) + "," + FloatToStringPrec(v.Y, 2) + "," + FloatToStringPrec(v.Z, 2);
}

function QuenFcRot(r : EulerAngles) : string
{
	return FloatToStringPrec(r.Pitch, 2) + "," + FloatToStringPrec(r.Yaw, 2) + "," + FloatToStringPrec(r.Roll, 2);
}

// The alive actors within radius of at, the player among them when in range (FindGameplayEntitiesInSphere with
// FLAG_OnlyAliveActors, as vanilla's GetActorsInRange asks).
function QuenFcCount(at : Vector, radius : float) : int
{
	var ents : array<CGameplayEntity>;

	FindGameplayEntitiesInSphere(ents, at, radius, 1000, '', FLAG_OnlyAliveActors);
	return ents.Size();
}

// One line: the free camera (on, its position), the rendered camera (theCamera: does it follow the free one?), the
// player, the camera's distance from him, the world's clocks and pause, and the alive actors within radius (default 30
// m, as world_actors) of the free camera and of the player.
function QuenFcStatus(what : string, radius : float)
{
	var fc, p : Vector;

	if (radius <= 0) radius = 30;
	fc = theGame.GetFreeCameraPosition();
	p = thePlayer.GetWorldPosition();
	LogChannel('QuenFC', what + " on=" + theGame.IsFreeCameraEnabled() + " fc=" + QuenFcPos(fc)
		+ " cam=" + QuenFcPos(theCamera.GetCameraPosition()) + " cam_rot=" + QuenFcRot(theCamera.GetCameraRotation())
		+ " player=" + QuenFcPos(p) + " heading=" + FloatToStringPrec(thePlayer.GetHeading(), 2)
		+ " dist=" + FloatToStringPrec(VecDistance(fc, p), 2) + " paused=" + theGame.IsPaused()
		+ " game_s=" + GameTimeToSeconds(theGame.GetGameTime()) + " engine_s=" + FloatToStringPrec(theGame.GetEngineTimeAsSeconds(), 2)
		+ " r=" + FloatToStringPrec(radius, 0) + " near_fc=" + QuenFcCount(fc, radius) + " near_player=" + QuenFcCount(p, radius));
}

// MoveFreeCamera, then the position read back at once (the move may land a tick later: qfcstatus reads it again).
function QuenFcMove(what : string, pos : Vector, rot : EulerAngles)
{
	var fc : Vector;

	theGame.MoveFreeCamera(pos, rot);
	fc = theGame.GetFreeCameraPosition();
	LogChannel('QuenFC', what + " sent=" + QuenFcPos(pos) + " rot=" + QuenFcRot(rot) + " on=" + theGame.IsFreeCameraEnabled()
		+ " read=" + QuenFcPos(fc) + " off_m=" + FloatToStringPrec(VecDistance(pos, fc), 2));
}

// qfcon() / qfcoff(): EnableFreeCamera(true / false), then the status line.
exec function qfcon()
{
	theGame.EnableFreeCamera(true);
	QuenFcStatus("on", 0);
}

exec function qfcoff()
{
	theGame.EnableFreeCamera(false);
	QuenFcStatus("off", 0);
}

// qfcmove(x, y, z, pitch, yaw, roll): the free camera to that pose (metres; degrees, EulerAngles(Pitch, Yaw, Roll)).
exec function qfcmove(x : float, y : float, z : float, optional pitch : float, optional yaw : float, optional roll : float)
{
	QuenFcMove("move", Vector(x, y, z), EulerAngles(pitch, yaw, roll));
}

// qfcrel(fwd, right, up): the free camera fwd m along Geralt's heading and right m to his right (both 0: fwd 10), up m
// (default 2) above the navmesh there (NavigationComputeZ within 30 m of his z; else above his z: ground= says which),
// aimed at 1.7 m above his position: yaw VecHeading (forward (-sin, cos), as game_camera's), pitch the drop's angle,
// negative looking down, as quen's photo-mode framing has it. The free camera's own convention is what the probe asks.
exec function qfcrel(optional fwd : float, optional right : float, optional up : float)
{
	var p, f, c, d : Vector;
	var z : float;
	var ground : string = "player";

	if (fwd == 0 && right == 0) fwd = 10;
	if (up == 0) up = 2;
	p = thePlayer.GetWorldPosition();
	f = VecNormalize2D(thePlayer.GetHeadingVector());
	c = p + fwd * f + right * Vector(f.Y, -f.X, 0);
	if (theGame.GetWorld().NavigationComputeZ(c, p.Z - 30, p.Z + 30, z))
	{
		c.Z = z;
		ground = "nav";
	}
	c.Z += up;
	d = p + Vector(0, 0, 1.7) - c;
	QuenFcMove("rel fwd=" + FloatToStringPrec(fwd, 1) + " right=" + FloatToStringPrec(right, 1) + " up=" + FloatToStringPrec(up, 1)
		+ " ground=" + ground, c, EulerAngles(Rad2Deg(AtanF(d.Z, VecLength2D(d))), VecHeading(d), 0));
}

// qfcstatus(radius): the status line, changing nothing.
exec function qfcstatus(optional radius : float)
{
	QuenFcStatus("status", radius);
}
