/*
Copyright (C) 1996-1997 Id Software, Inc.

This program is free software; you can redistribute it and/or
modify it under the terms of the GNU General Public License
as published by the Free Software Foundation; either version 2
of the License, or (at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.

See the GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program; if not, write to the Free Software
Foundation, Inc., 59 Temple Place - Suite 330, Boston, MA  02111-1307, USA.

*/
// walk_oracle.c -- a scripted walk through id's game, and a log of every call
// into the sound layer on the way (oracle/sound_walk.py runs the same walk
// through the port's client, `quaketool sndwalk`, and compares the logs).
//
//   oracle_walk path      walk path's script from the first frame the client
//                         sends a move; quit when it is done
//   oracle_sndlog path    log to path from now on (no argument: stop)
//
// The script: one segment a line, "frames yaw forward jump" ('#' starts a
// comment). Each host frame in which the client sends a move (CL_SendCmd,
// wrapped at link time, with cls.signon == SIGNONS) takes the next frame of it:
// cl.viewangles = (0, yaw, 0), as a mouse would leave them, and +forward and
// +jump pressed or released, as the console's own commands -- so the frame a
// key goes down moves at CL_KeyState's half. Frames of the level change's
// signon, which send no move, take none.
//
// The log: the build wraps the null sound driver's entry points (snd_null.c,
// -Wl,--wrap=S_StartSound and the rest, build.sh) and the server's
// SV_StartSound; each wrapper writes a line, then calls the function it
// wraps, so a run without oracle_sndlog is the oracle as before. snd_null's
// S_PrecacheSound returns NULL, which would leave the client nothing to name a
// sample by; its wrapper returns a named sfx_t, which only these wrappers read
// (the client passes the pointer through and never tests it).
//
// A line: the host frame (host_framecount), realtime, cl.time, sv.time (-1
// without a server), cls.signon, whether the local client is spawned, then
//   walk n x y z                           walk frame n starts; the player's origin
//   start ent class chan sample x y z vol attn   S_StartSound: class is the
//                                          server edict's classname (- for none),
//                                          vol and attn the wire bytes (vol*255, attn*64)
//   stop ent chan                          S_StopSound
//   stopall clear                          S_StopAllSounds
//   static sample x y z vol attn           S_StaticSound (the wire bytes)
//   local sample                           S_LocalSound
//   sv ent chan sample vol attn            SV_StartSound: what the server started

#include "quakedef.h"

static FILE		*sndlog;

#define ORACLE_MAX_SFX	512		// snd_dma.c's MAX_SFX
static sfx_t	oracle_sfx[ORACLE_MAX_SFX];
static int		oracle_num_sfx;

typedef struct
{
	int			frames;
	float		yaw;
	qboolean	forward, jump;
} walkseg_t;

#define MAX_WALK_SEGS	256
static walkseg_t	walk[MAX_WALK_SEGS];
static int			walk_segs, walk_seg, walk_left, walk_frame;
static qboolean		walking, forward_down, jump_down;

sfx_t *__real_S_PrecacheSound (char *sample);
void __real_S_StartSound (int entnum, int entchannel, sfx_t *sfx, vec3_t origin, float fvol, float attenuation);
void __real_S_StopSound (int entnum, int entchannel);
void __real_S_StopAllSounds (qboolean clear);
void __real_S_StaticSound (sfx_t *sfx, vec3_t origin, float vol, float attenuation);
void __real_S_LocalSound (char *s);
void __real_SV_StartSound (edict_t *entity, int channel, char *sample, int volume, float attenuation);
void __real_CL_SendCmd (void);

static void Oracle_SndLog_f (void)
{
	if (sndlog)
		fclose (sndlog);
	sndlog = NULL;
	if (Cmd_Argc () < 2)
		return;
	sndlog = fopen (Cmd_Argv (1), "w");
	if (!sndlog)
		Sys_Error ("oracle_sndlog: cannot write %s", Cmd_Argv (1));
}

static void Oracle_Walk_f (void)
{
	FILE		*f;
	char		line[256];
	walkseg_t	s;
	int			fwd, jump;

	if (Cmd_Argc () != 2)
	{
		Con_Printf ("oracle_walk path\n");
		return;
	}
	f = fopen (Cmd_Argv (1), "r");
	if (!f)
		Sys_Error ("oracle_walk: cannot read %s", Cmd_Argv (1));
	walk_segs = 0;
	while (fgets (line, sizeof(line), f))
	{
		if (sscanf (line, "%d %f %d %d", &s.frames, &s.yaw, &fwd, &jump) != 4 || s.frames <= 0)
			continue;	// a comment or a blank line
		if (walk_segs == MAX_WALK_SEGS)
			Sys_Error ("oracle_walk: more than %d segments", MAX_WALK_SEGS);
		s.forward = fwd != 0;
		s.jump = jump != 0;
		walk[walk_segs++] = s;
	}
	fclose (f);
	walk_seg = walk_frame = 0;
	walk_left = walk_segs ? walk[0].frames : 0;
	walking = true;
}

void Oracle_Walk_Init (void)
{
	Cmd_AddCommand ("oracle_sndlog", Oracle_SndLog_f);
	Cmd_AddCommand ("oracle_walk", Oracle_Walk_f);
}

// The line's head: the host frame and the clocks.
static qboolean SndLog_Begin (void)
{
	if (!sndlog)
		return false;
	fprintf (sndlog, "%d %.6f %.6f %.6f %d %d ", host_framecount, realtime, cl.time,
		sv.active ? sv.time : -1.0, cls.signon, svs.clients && svs.clients[0].spawned);
	return true;
}

// A key command as the console types it (KeyDown/KeyUp with no key number).
static void Walk_Key (char *cmd)
{
	char	text[32];

	Q_strncpy (text, cmd, sizeof(text)-1);
	Cmd_ExecuteString (text, src_command);
}

// One frame of the script: the angles and keys this frame's move is built from.
static void Walk_Step (void)
{
	walkseg_t	*s;
	float		*org;

	if (walk_seg == walk_segs)
	{
		if (sndlog)
			fclose (sndlog);
		sndlog = NULL;
		Sys_Quit ();
	}
	s = &walk[walk_seg];
	if (SndLog_Begin ())
	{
		org = svs.clients[0].edict->v.origin;
		fprintf (sndlog, "walk %d %.3f %.3f %.3f\n", walk_frame, org[0], org[1], org[2]);
	}
	cl.viewangles[PITCH] = 0;
	cl.viewangles[YAW] = s->yaw;
	cl.viewangles[ROLL] = 0;
	if (s->forward != forward_down)
		Walk_Key (s->forward ? "+forward" : "-forward");
	if (s->jump != jump_down)
		Walk_Key (s->jump ? "+jump" : "-jump");
	forward_down = s->forward;
	jump_down = s->jump;
	walk_frame++;
	if (--walk_left == 0 && ++walk_seg < walk_segs)
		walk_left = walk[walk_seg].frames;
}

void __wrap_CL_SendCmd (void)
{
	if (walking && cls.state == ca_connected && cls.signon == SIGNONS && sv.active)
		Walk_Step ();
	__real_CL_SendCmd ();
}

static char *SndLog_Name (sfx_t *sfx)
{
	return sfx ? sfx->name : "(null)";
}

// The server edict's classname, for an entity number the client was sent.
static char *SndLog_Class (int entnum)
{
	edict_t	*e;

	if (!sv.active || entnum <= 0 || entnum >= sv.num_edicts)
		return "-";
	e = EDICT_NUM(entnum);
	if (e->free || !e->v.classname || !pr_strings[e->v.classname])
		return "-";
	return pr_strings + e->v.classname;
}

sfx_t *__wrap_S_PrecacheSound (char *sample)
{
	int		i;

	__real_S_PrecacheSound (sample);
	for (i=0 ; i<oracle_num_sfx ; i++)
		if (!Q_strcmp (oracle_sfx[i].name, sample))
			return &oracle_sfx[i];
	if (oracle_num_sfx == ORACLE_MAX_SFX)
		return NULL;
	Q_strncpy (oracle_sfx[oracle_num_sfx].name, sample, MAX_QPATH-1);
	return &oracle_sfx[oracle_num_sfx++];
}

void __wrap_S_StartSound (int entnum, int entchannel, sfx_t *sfx, vec3_t origin, float fvol, float attenuation)
{
	if (SndLog_Begin ())
		fprintf (sndlog, "start %d %s %d %s %.3f %.3f %.3f %d %d\n", entnum, SndLog_Class (entnum), entchannel,
			SndLog_Name (sfx), origin[0], origin[1], origin[2], (int)(fvol*255 + 0.5), (int)(attenuation*64 + 0.5));
	__real_S_StartSound (entnum, entchannel, sfx, origin, fvol, attenuation);
}

void __wrap_S_StopSound (int entnum, int entchannel)
{
	if (SndLog_Begin ())
		fprintf (sndlog, "stop %d %d\n", entnum, entchannel);
	__real_S_StopSound (entnum, entchannel);
}

void __wrap_S_StopAllSounds (qboolean clear)
{
	if (SndLog_Begin ())
		fprintf (sndlog, "stopall %d\n", (int)clear);
	__real_S_StopAllSounds (clear);
}

void __wrap_S_StaticSound (sfx_t *sfx, vec3_t origin, float vol, float attenuation)
{
	if (SndLog_Begin ())
		fprintf (sndlog, "static %s %.3f %.3f %.3f %d %d\n", SndLog_Name (sfx),
			origin[0], origin[1], origin[2], (int)vol, (int)attenuation);
	__real_S_StaticSound (sfx, origin, vol, attenuation);
}

void __wrap_S_LocalSound (char *s)
{
	if (SndLog_Begin ())
		fprintf (sndlog, "local %s\n", s);
	__real_S_LocalSound (s);
}

void __wrap_SV_StartSound (edict_t *entity, int channel, char *sample, int volume, float attenuation)
{
	if (SndLog_Begin ())
		fprintf (sndlog, "sv %d %d %s %d %d\n", NUM_FOR_EDICT (entity), channel, sample, volume,
			(int)(attenuation*64 + 0.5));
	__real_SV_StartSound (entity, channel, sample, volume, attenuation);
}
