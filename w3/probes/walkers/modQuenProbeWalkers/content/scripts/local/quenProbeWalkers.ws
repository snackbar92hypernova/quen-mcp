// modQuenProbeWalkers: a PROBE for quen's walker scenarios, not for play (README.md beside this mod).
// Do spawned commoners, not a community's, walk where a script tells them? Console execs spawn N in a lane through
// or beside Geralt, order them to its far end, log each walk on LogChannel('QuenWalk') and destroy them by tag.
// Nothing runs until typed. No resources, no wrapped vanilla method; one added timer on the player.

class QuenWalker
{
	var npc : CNewNPC;
	var start, target, last : Vector;  // the walk's line (from where it stood at its start), the last sample
	var t0, t1, timeout : float;       // engine time of the walk's start (while waiting, when it is due) and of
	                                   // arrival or failure; the walk's limit
	var path, dev : float;             // horizontal: length walked, largest distance from the start-target line
	var phase, method : int;           // phase 0 spawned, 1 walking, 2 arrived, 3 failed, 4 ordered (waits for its start)
}

@addField(CR4Player) var quenWalkers : array<QuenWalker>;

function QuenWalkPos(v : Vector) : string
{
	return FloatToStringPrec(v.X, 2) + "," + FloatToStringPrec(v.Y, 2) + "," + FloatToStringPrec(v.Z, 2);
}

// qwspawn(n, dist, side): clears the lane, then spawns n walkers (default 3, at most 12) dist m ahead of Geralt
// (default 10), 1.2 m apart across the lane, its middle side m to his right (default 0: through his spot). Each one's
// target is the mirror point dist m behind him; both Z from the navmesh. The lane is set by Geralt's position and
// heading, so a teleport to the same pose gives the same lane.
exec function qwspawn(optional n : int, optional dist : float, optional side : float)
{
	// The non-community copy of the Novigrad commoner: a CNewNPC on npc_base.w2behtree, group ENGT_Commoner, the same
	// AI trees as crowd_npc\novigrad_citizen\community\novigrad_citizen_community.w2ent (quen data_read, 2026-10-20).
	var path : string = "characters\npc_entities\crowd_npc\novigrad_citizen\novigrad_citizen.w2ent";
	var world : CWorld = theGame.GetWorld();
	var tpl : CEntityTemplate;
	var tags : array<name>;
	var w : QuenWalker;
	var npc : CNewNPC;
	var p, f, r, a, b : Vector;
	var lat, z : float;
	var i : int;

	if (n <= 0) n = 3;
	if (n > 12) n = 12;  // the design's cap for a run
	if (dist <= 0) dist = 10;
	QuenWalkersClear();
	tpl = (CEntityTemplate)LoadResource(path, true);
	if (!tpl)
	{
		LogChannel('QuenWalk', "spawn refused: template not loaded " + path);
		return;
	}
	p = thePlayer.GetWorldPosition();
	f = VecNormalize2D(thePlayer.GetHeadingVector());  // forward (-sin h, cos h)
	r = Vector(f.Y, -f.X, 0);                           // its right
	tags.PushBack('quen_walker');
	for (i = 0; i < n; i += 1)
	{
		lat = side + ((float)i - (float)(n - 1) * 0.5) * 1.2;
		a = p + dist * f + lat * r;  // ahead, walking towards the camera
		b = p - dist * f + lat * r;
		if (!world.NavigationComputeZ(a, p.Z - 5, p.Z + 5, z))
		{
			LogChannel('QuenWalk', "spawn lane=" + i + " skipped: start off the navmesh at=" + QuenWalkPos(a));
			continue;
		}
		a.Z = z;
		if (!world.NavigationComputeZ(b, p.Z - 5, p.Z + 5, z))
		{
			LogChannel('QuenWalk', "spawn lane=" + i + " skipped: target off the navmesh at=" + QuenWalkPos(b));
			continue;
		}
		b.Z = z;
		// vanilla's idiom for test NPCs (animationTestSpawner.ws), the tag set at creation
		npc = (CNewNPC)theGame.CreateEntity(tpl, a, EulerAngles(0, VecHeading(b - a), 0), true, false, false, PM_DontPersist, tags);
		if (!npc)
		{
			LogChannel('QuenWalk', "spawn lane=" + i + " failed: CreateEntity gave no CNewNPC");
			continue;
		}
		w = new QuenWalker in thePlayer;
		w.npc = npc;
		w.start = a;
		w.target = b;
		thePlayer.quenWalkers.PushBack(w);
		LogChannel('QuenWalk', "spawn i=" + (thePlayer.quenWalkers.Size() - 1) + " lane=" + i + " at=" + QuenWalkPos(a) + " to=" + QuenWalkPos(b)
			+ " lane_clear=" + world.NavigationLineTest(a, b, 0.4));
	}
	LogChannel('QuenWalk', "spawned " + thePlayer.quenWalkers.Size() + " of " + n + " player=" + QuenWalkPos(p)
		+ " heading=" + FloatToStringPrec(thePlayer.GetHeading(), 2) + " template=" + path);
	theGame.GetGuiManager().ShowNotification("quen walkers: " + thePlayer.quenWalkers.Size() + " of " + n + " spawned", 8000);
}

// qwgo(method, timeout, delay): orders every walker that isn't walking or waiting to its target. The walks start
// delay s later (default 0), all on one tick, each from where its walker stands then: so a trace started after this
// line sees the whole walk. method 0 (default): a CAIMoveToPoint tree forced at BTAP_Emergency, as the scene player
// walks NPCs (scenePlayer.ws) and the MovePlayerFwd exec builds it (temp.ws). method 1: ActionMoveToAsync, which
// community NPCs refused in boot 178. A walk fails timeout s after its start (default 60).
exec function qwgo(optional method : int, optional timeout : float, optional delay : float)
{
	var w : QuenWalker;
	var i, n : int;

	if (timeout <= 0) timeout = 60;
	for (i = 0; i < thePlayer.quenWalkers.Size(); i += 1)
	{
		w = thePlayer.quenWalkers[i];
		if (w.phase == 1 || w.phase == 4)
			continue;
		w.method = method;
		w.timeout = timeout;
		w.t0 = theGame.GetEngineTimeAsSeconds() + delay;
		w.phase = 4;
		n += 1;
	}
	if (n > 0)
		thePlayer.AddTimer('QuenWalkersTick', 0.2, true, , , , true);
	LogChannel('QuenWalk', "ordered " + n + " method=" + method + " timeout=" + FloatToStringPrec(timeout, 0) + " delay=" + FloatToStringPrec(delay, 1));
}

// Every 0.2 s while a walk runs or waits: starts the walks that are due; path and deviation per walker; arrival
// within 1.0 m (horizontal) of its target, the CAIMoveToPoint default maxDistance; a failure at the timeout or when
// the walker is gone. When none walks or waits, the status.
@addMethod(CR4Player) timer function QuenWalkersTick(dt : float, id : int)
{
	var tree : CAIMoveToPoint;
	var w : QuenWalker;
	var pos, ab : Vector;
	var t, len : float;
	var i, oid, walking : int;

	t = theGame.GetEngineTimeAsSeconds();
	for (i = 0; i < quenWalkers.Size(); i += 1)
	{
		w = quenWalkers[i];
		if (w.phase == 4)
		{
			if (t < w.t0)
			{
				walking += 1;
				continue;
			}
			if (!w.npc || !w.npc.IsAlive())
			{
				w.phase = 3;
				w.t1 = w.t0;
				LogChannel('QuenWalk', "order i=" + i + " failed: gone");
				continue;
			}
			oid = 0;  // ForceAIBehavior's action id, or ActionMoveToAsync's bool as 0 or 1
			if (w.method == 1)
			{
				if (w.npc.ActionMoveToAsync(w.target, MT_Walk, 1.0, 1.0))
					oid = 1;
			}
			else
			{
				tree = new CAIMoveToPoint in w.npc;
				tree.OnCreated();
				tree.enterExplorationOnStart = false;
				tree.params.moveType = MT_Walk;
				tree.params.destinationPosition = w.target;
				tree.params.destinationHeading = VecHeading(w.target - w.npc.GetWorldPosition());
				oid = w.npc.ForceAIBehavior(tree, BTAP_Emergency);
			}
			w.start = w.npc.GetWorldPosition();  // the walk's line and clock start here
			w.last = w.start;
			w.t0 = t;
			w.path = 0;
			w.dev = 0;
			w.phase = 1;
			walking += 1;
			LogChannel('QuenWalk', "order i=" + i + " method=" + w.method + " id=" + oid + " from=" + QuenWalkPos(w.start)
				+ " dist=" + FloatToStringPrec(VecDistance2D(w.start, w.target), 2));
			continue;
		}
		if (w.phase != 1)
			continue;
		if (!w.npc || !w.npc.IsAlive())
		{
			w.phase = 3;
			w.t1 = t;
			LogChannel('QuenWalk', "failed i=" + i + " why=gone t=" + FloatToStringPrec(t - w.t0, 2));
			continue;
		}
		pos = w.npc.GetWorldPosition();
		w.path += VecDistance2D(w.last, pos);
		w.last = pos;
		ab = w.target - w.start;
		len = VecLength2D(ab);
		if (len > 0.01)
			w.dev = MaxF(w.dev, AbsF(ab.X * (pos.Y - w.start.Y) - ab.Y * (pos.X - w.start.X)) / len);
		if (VecDistance2D(pos, w.target) <= 1.0)
		{
			w.phase = 2;
			w.t1 = t;
			LogChannel('QuenWalk', "arrived i=" + i + " t=" + FloatToStringPrec(t - w.t0, 2) + " dist=" + FloatToStringPrec(VecDistance2D(pos, w.target), 2)
				+ " path=" + FloatToStringPrec(w.path, 2) + " straight=" + FloatToStringPrec(len, 2) + " dev=" + FloatToStringPrec(w.dev, 2));
		}
		else if (t - w.t0 > w.timeout)
		{
			w.phase = 3;
			w.t1 = t;
			LogChannel('QuenWalk', "failed i=" + i + " why=timeout t=" + FloatToStringPrec(t - w.t0, 2) + " dist=" + FloatToStringPrec(VecDistance2D(pos, w.target), 2)
				+ " path=" + FloatToStringPrec(w.path, 2) + " straight=" + FloatToStringPrec(len, 2) + " dev=" + FloatToStringPrec(w.dev, 2) + " at=" + QuenWalkPos(pos));
		}
		else
			walking += 1;
	}
	if (walking == 0)
	{
		RemoveTimer('QuenWalkersTick');
		QuenWalkersStatus();
	}
}

// qwstatus(): one line per walker this player spawned (state, time, distance to target now, path, deviation, where),
// then the totals and how many entities in the world carry the tag. After a load the list is empty, so the tag count
// is the persistence check.
exec function qwstatus()
{
	QuenWalkersStatus();
}

function QuenWalkersStatus()
{
	var states : array<string>;
	var ents : array<CEntity>;
	var w : QuenWalker;
	var pos : Vector;
	var t : float;
	var i : int;
	var counts : array<int>;

	states.PushBack("spawned");
	states.PushBack("walking");
	states.PushBack("arrived");
	states.PushBack("failed");
	states.PushBack("waiting");
	counts.Resize(5);
	for (i = 0; i < thePlayer.quenWalkers.Size(); i += 1)
	{
		w = thePlayer.quenWalkers[i];
		counts[w.phase] += 1;
		if (!w.npc)
		{
			LogChannel('QuenWalk', "status i=" + i + " state=" + states[w.phase] + " gone");
			continue;
		}
		pos = w.npc.GetWorldPosition();
		t = w.t1;
		if (w.phase == 1 || w.phase == 4)
			t = theGame.GetEngineTimeAsSeconds();
		LogChannel('QuenWalk', "status i=" + i + " state=" + states[w.phase] + " t=" + FloatToStringPrec(t - w.t0, 2)
			+ " dist=" + FloatToStringPrec(VecDistance2D(pos, w.target), 2) + " path=" + FloatToStringPrec(w.path, 2)
			+ " straight=" + FloatToStringPrec(VecDistance2D(w.start, w.target), 2) + " dev=" + FloatToStringPrec(w.dev, 2) + " at=" + QuenWalkPos(pos));
	}
	theGame.GetEntitiesByTag('quen_walker', ents);
	LogChannel('QuenWalk', "status tracked=" + thePlayer.quenWalkers.Size() + " spawned=" + counts[0] + " walking=" + counts[1]
		+ " arrived=" + counts[2] + " failed=" + counts[3] + " waiting=" + counts[4] + " tagged=" + ents.Size());
	theGame.GetGuiManager().ShowNotification("quen walkers: " + counts[2] + " arrived, " + counts[3] + " failed, " + ents.Size() + " tagged", 8000);
}

// qwclear(): destroys every entity tagged quen_walker, stops the tick and forgets the list. Idempotent.
exec function qwclear()
{
	QuenWalkersClear();
}

function QuenWalkersClear()
{
	var ents : array<CEntity>;
	var i : int;

	thePlayer.RemoveTimer('QuenWalkersTick');
	theGame.GetEntitiesByTag('quen_walker', ents);
	for (i = 0; i < ents.Size(); i += 1)
		ents[i].Destroy();
	thePlayer.quenWalkers.Clear();
	LogChannel('QuenWalk', "clear destroyed=" + ents.Size());
}
