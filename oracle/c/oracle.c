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
// oracle.c -- the quake-rust oracle instrument: console commands that pin the
// view and dump the frame id's own software renderer draws.
//
// The link wraps R_RenderView (-Wl,--wrap=R_RenderView), so every call view.c
// makes lands in __wrap_R_RenderView below, which can substitute the view and
// clock for one frame and dump vid.buffer the instant the 3-D view is finished
// (before sbar, console, notify text, centerprints or the menu touch it).
// Nothing in id's tree is edited except quakedef.h's id386 switch (build.sh).
//
// Commands (all take effect for shots queued AFTER them):
//   oracle_view x y z pitch yaw roll   pin r_refdef.vieworg/viewangles (Quake
//                                      convention: pitch + looks down); no args = unpin
//   oracle_time t                      pin cl.time for the shot's render (light
//                                      styles, sky, turb, texture/alias anims); no args = unpin
//   oracle_stage 0|1                   0 = dump after R_RenderView (3-D only, default),
//                                      1 = dump at VID_Update (the composited screen)
//   oracle_settle n                    rendered frames to let pass after signon before
//                                      the first shot (default 0: the first frame)
//   oracle_exit 0|1                    quit once every queued shot is written (default 1),
//                                      and when a timedemo finishes
//   oracle_shot path                   queue a shot: writes path.pgm (raw palette
//                                      indices), path.ppm (RGB), path.json (the view,
//                                      clock, vrect, light styles, ...) and path.ents
//                                      (the entities the frame drew, one per line)
//   oracle_spans 8|16|1                 (a cvar) the textured-span routine: 8 = id's portable
//                                      C D_DrawSpans8 (default), 16 = D_DrawSpans16's 16-pixel
//                                      subdivision (the x86 asm default, d_subdiv16 1), 1 = exact
//                                      per-pixel perspective (an attribution experiment, not id)
//   oracle_bench n                     (a cvar) render each shot n more times first and report
//                                      the warm ms/frame (renderer only)
//
// The 2-D layer (oracle/screen2d.py drives these; see oracle/README.md):
//   oracle_blank idx                   (a cvar, default -1 = off) fill the 3-D view rectangle
//                                      (scr_vrect) with palette index idx after every
//                                      R_RenderView, so a composited shot measures the 2-D
//                                      layer alone (sbar, menus, console, text, backtile, fade)
//   oracle_field name value...         set a field of the player's edict (ED_ParseEpair):
//                                      health, items, weapon, ammo_shells, ...
//   oracle_global name value...        set a QuakeC global the same way (serverflags, ...)
//   oracle_centerprint text...         SCR_CenterPrint; a literal \n is a newline
//   oracle_intermission n t [text...]  what svc_intermission (n 1), svc_finale (2) and
//                                      svc_cutscene (3) do on the client, with
//                                      cl.completed_time = t and the text centerprinted
//   oracle_faceanim                    cl.faceanimtime = cl.time + 0.2 (V_ParseDamage's pain face)
//   oracle_key keyname                 Key_Event down + up (keys.c names: ESCAPE, ENTER,
//                                      UPARROW, DOWNARROW, TAB, a single character, ...)
// A shot's .json also carries the 2-D clocks and state (realtime, host_time,
// scr_centertime_start, scr_con_current, key_dest, cl.stats, ...).

#include "quakedef.h"
#include "r_local.h"
#include "d_local.h"
#include <time.h>

extern cvar_t	r_drawviewmodel;
extern cvar_t	scr_fov;
extern byte		oracle_palette[768];

void __real_R_RenderView (void);

#define MAX_ORACLE_SHOTS	64
#define ORACLE_MAXPATH		1024	// MAX_OSPATH (128) truncates real scratch paths

typedef struct
{
	char		path[ORACLE_MAXPATH];
	qboolean	has_view;
	vec3_t		org, ang;
	qboolean	has_time;
	double		time;
	int			stage;
} oracle_shot_t;

static oracle_shot_t	shots[MAX_ORACLE_SHOTS];
static int				numshots, curshot;

static oracle_shot_t	pending;		// settings the next oracle_shot captures
static int				settle;
static qboolean			exit_when_done = true;

static int				rendered;		// post-signon R_RenderView calls so far
static oracle_shot_t	*active;		// shot being rendered this frame
static qboolean			full_pending;	// active stage-1 shot waiting for VID_Update
static double			natural_time;	// cl.time before any override
static double			used_time;		// cl.time the active shot rendered with
static vec3_t			natural_org, natural_ang;
static qboolean			timedemo_seen;

extern cvar_t	oracle_spans;

// the 2-D layer instrument
cvar_t		oracle_blank = {"oracle_blank", "-1"};
extern double	host_time;
extern float	scr_centertime_start, scr_centertime_off;
extern int		m_state;
ddef_t		*ED_FindField (char *name);
ddef_t		*ED_FindGlobal (char *name);
qboolean	ED_ParseEpair (void *base, ddef_t *key, char *s);
int			Key_StringToKeynum (char *str);

// oracle_bench N: after a shot's (cold) render, render the same view N more
// times and report the warm ms/frame -- renderer only, like quaketool's --bench.
cvar_t		oracle_bench = {"oracle_bench", "0"};
static double	bench_ms;
static int		bench_frames;

static void Oracle_View_f (void)
{
	int		i;

	if (Cmd_Argc () == 1)
	{
		pending.has_view = false;
		return;
	}
	if (Cmd_Argc () != 7)
	{
		Con_Printf ("oracle_view x y z pitch yaw roll\n");
		return;
	}
	for (i=0 ; i<3 ; i++)
	{
		pending.org[i] = Q_atof (Cmd_Argv (1+i));
		pending.ang[i] = Q_atof (Cmd_Argv (4+i));
	}
	pending.has_view = true;
}

static void Oracle_Time_f (void)
{
	if (Cmd_Argc () == 1)
	{
		pending.has_time = false;
		return;
	}
	pending.time = Q_atof (Cmd_Argv (1));
	pending.has_time = true;
}

static void Oracle_Stage_f (void)
{
	if (Cmd_Argc () == 2)
		pending.stage = Q_atoi (Cmd_Argv (1));
}

static void Oracle_Settle_f (void)
{
	if (Cmd_Argc () == 2)
		settle = Q_atoi (Cmd_Argv (1));
}

static void Oracle_Exit_f (void)
{
	if (Cmd_Argc () == 2)
		exit_when_done = Q_atoi (Cmd_Argv (1)) != 0;
}

static void Oracle_Shot_f (void)
{
	if (Cmd_Argc () != 2)
	{
		Con_Printf ("oracle_shot path\n");
		return;
	}
	if (numshots == MAX_ORACLE_SHOTS)
	{
		Con_Printf ("oracle_shot: queue full\n");
		return;
	}
	shots[numshots] = pending;
	Q_strncpy (shots[numshots].path, Cmd_Argv (1), ORACLE_MAXPATH-1);
	numshots++;
}

// oracle_edicts path -- the census instrument: append every live server edict
// (number, classname, model, origin, angles, frame, movetype, solid, flags,
// health, nextthink, effects, targetname) to path, preceded by a "# t=" line
// with sv.time. Lets a port run be diffed against id's own simulation.
static void Oracle_Edicts_f (void)
{
	FILE	*f;
	edict_t	*e;
	int		i;

	if (Cmd_Argc () != 2 || !sv.active)
	{
		Con_Printf ("oracle_edicts path (needs a running server)\n");
		return;
	}
	f = fopen (Cmd_Argv (1), "a");
	if (!f)
	{
		Con_Printf ("oracle_edicts: can't open %s\n", Cmd_Argv (1));
		return;
	}
	fprintf (f, "# t=%.3f num_edicts=%d\n", sv.time, sv.num_edicts);
	for (i=0 ; i<sv.num_edicts ; i++)
	{
		e = EDICT_NUM(i);
		if (e->free)
			continue;
		fprintf (f, "%d\t%s\t%s\t%.3f %.3f %.3f\t%.3f %.3f %.3f\t%g\t%g\t%g\t%g\t%g\t%.3f\t%g\t%s\n", i,
			pr_strings + e->v.classname, pr_strings + e->v.model,
			e->v.origin[0], e->v.origin[1], e->v.origin[2],
			e->v.angles[0], e->v.angles[1], e->v.angles[2],
			e->v.frame, e->v.movetype, e->v.solid, e->v.flags, e->v.health,
			e->v.nextthink, e->v.effects, pr_strings + e->v.targetname);
	}
	fclose (f);
}

// oracle_client path -- append the client-side state a census compares: cl.time,
// the view angles, the four cshift percents (contents/damage/bonus/powerup) and
// their colours, the punch angle, and the stats.
static void Oracle_Client_f (void)
{
	FILE	*f;
	int		i;

	if (Cmd_Argc () != 2)
	{
		Con_Printf ("oracle_client path\n");
		return;
	}
	f = fopen (Cmd_Argv (1), "a");
	if (!f)
		return;
	fprintf (f, "t=%.3f viewangles=%.3f %.3f %.3f punch=%.3f %.3f %.3f idealpitch=%.3f onground=%d intermission=%d",
		cl.time, cl.viewangles[0], cl.viewangles[1], cl.viewangles[2],
		cl.punchangle[0], cl.punchangle[1], cl.punchangle[2], cl.idealpitch, cl.onground, cl.intermission);
	for (i=0 ; i<NUM_CSHIFTS ; i++)
		fprintf (f, " cshift%d=%d,%d,%d@%d", i, cl.cshifts[i].destcolor[0], cl.cshifts[i].destcolor[1],
			cl.cshifts[i].destcolor[2], cl.cshifts[i].percent);
	fprintf (f, " stats=");
	for (i=0 ; i<16 ; i++)
		fprintf (f, "%d,", cl.stats[i]);
	fprintf (f, "\n");
	fclose (f);
}

// oracle_quit -- Sys_Quit now (the stock `quit` opens the M_Menu_Quit confirm
// unless the console is down, which a script run never has).
static void Oracle_Quit_f (void)
{
	Sys_Quit ();
}

// The arguments from argv[first] on, joined by single spaces.
static char *Oracle_ArgsFrom (int first)
{
	static char	buf[1024];
	int			i;

	buf[0] = 0;
	for (i=first ; i<Cmd_Argc () ; i++)
	{
		if (i > first)
			Q_strcat (buf, " ");
		if (strlen (buf) + strlen (Cmd_Argv (i)) + 2 >= sizeof(buf))
			break;
		Q_strcat (buf, Cmd_Argv (i));
	}
	return buf;
}

// A literal backslash-n becomes a newline (the console tokenizer has no escapes).
static char *Oracle_Unescape (char *s)
{
	char	*r, *w;

	for (r = w = s ; *r ; r++)
	{
		if (r[0] == '\\' && r[1] == 'n')
		{
			*w++ = '\n';
			r++;
		}
		else
			*w++ = *r;
	}
	*w = 0;
	return s;
}

// oracle_field name value... -- set a field of the player's edict
static void Oracle_Field_f (void)
{
	ddef_t	*def;

	if (Cmd_Argc () < 3 || !sv.active)
	{
		Con_Printf ("oracle_field name value (needs a running server)\n");
		return;
	}
	def = ED_FindField (Cmd_Argv (1));
	if (!def)
	{
		Con_Printf ("oracle_field: no field %s\n", Cmd_Argv (1));
		return;
	}
	ED_ParseEpair ((void *)&svs.clients[0].edict->v, def, Oracle_ArgsFrom (2));
}

// oracle_global name value... -- set a QuakeC global
static void Oracle_Global_f (void)
{
	ddef_t	*def;

	if (Cmd_Argc () < 3 || !sv.active)
	{
		Con_Printf ("oracle_global name value (needs a running server)\n");
		return;
	}
	def = ED_FindGlobal (Cmd_Argv (1));
	if (!def)
	{
		Con_Printf ("oracle_global: no global %s\n", Cmd_Argv (1));
		return;
	}
	ED_ParseEpair ((void *)pr_globals, def, Oracle_ArgsFrom (2));
}

static void Oracle_CenterPrint_f (void)
{
	SCR_CenterPrint (Oracle_Unescape (Oracle_ArgsFrom (1)));
}

// oracle_intermission n t [text] -- cl_parse.c's svc_intermission / svc_finale /
// svc_cutscene, with the completion time given
static void Oracle_Intermission_f (void)
{
	if (Cmd_Argc () < 3)
	{
		Con_Printf ("oracle_intermission n completed_time [text]\n");
		return;
	}
	cl.intermission = Q_atoi (Cmd_Argv (1));
	cl.completed_time = Q_atof (Cmd_Argv (2));
	vid.recalc_refdef = true;	// go to full screen
	if (cl.intermission >= 2)
		SCR_CenterPrint (Oracle_Unescape (Oracle_ArgsFrom (3)));
}

static void Oracle_FaceAnim_f (void)
{
	cl.faceanimtime = cl.time + 0.2;
}

static void Oracle_Key_f (void)
{
	int		k;

	if (Cmd_Argc () != 2)
	{
		Con_Printf ("oracle_key keyname\n");
		return;
	}
	k = Key_StringToKeynum (Cmd_Argv (1));
	if (k < 0)
	{
		Con_Printf ("oracle_key: unknown key %s\n", Cmd_Argv (1));
		return;
	}
	Key_Event (k, true);
	Key_Event (k, false);
}

void Oracle_Init (void)
{
	Cmd_AddCommand ("oracle_field", Oracle_Field_f);
	Cmd_AddCommand ("oracle_global", Oracle_Global_f);
	Cmd_AddCommand ("oracle_centerprint", Oracle_CenterPrint_f);
	Cmd_AddCommand ("oracle_intermission", Oracle_Intermission_f);
	Cmd_AddCommand ("oracle_faceanim", Oracle_FaceAnim_f);
	Cmd_AddCommand ("oracle_key", Oracle_Key_f);
	Cvar_RegisterVariable (&oracle_blank);
	Cmd_AddCommand ("oracle_quit", Oracle_Quit_f);
	Cmd_AddCommand ("oracle_edicts", Oracle_Edicts_f);
	Cmd_AddCommand ("oracle_client", Oracle_Client_f);
	Cmd_AddCommand ("oracle_view", Oracle_View_f);
	Cmd_AddCommand ("oracle_time", Oracle_Time_f);
	Cmd_AddCommand ("oracle_stage", Oracle_Stage_f);
	Cmd_AddCommand ("oracle_settle", Oracle_Settle_f);
	Cmd_AddCommand ("oracle_exit", Oracle_Exit_f);
	Cmd_AddCommand ("oracle_shot", Oracle_Shot_f);
	Cvar_RegisterVariable (&oracle_spans);
	Cvar_RegisterVariable (&oracle_bench);
}

//=============================================================================

static FILE *Oracle_Open (char *path, char *ext)
{
	char	name[ORACLE_MAXPATH + 8];
	FILE	*f;

	sprintf (name, "%s.%s", path, ext);
	f = fopen (name, "wb");
	if (!f)
		Sys_Error ("oracle: cannot write %s", name);
	return f;
}

static char *Oracle_ModelName (entity_t *e)
{
	return e->model ? e->model->name : "";
}

static void Oracle_WriteVec (FILE *f, char *key, float *v)
{
	fprintf (f, "  \"%s\": [%.9g, %.9g, %.9g],\n", key, v[0], v[1], v[2]);
}

static void Oracle_Dump (oracle_shot_t *s, int stage)
{
	FILE	*f;
	int		x, y, i, n;
	byte	*pal, *row, rgb[3];
	entity_t	*e;

// raw 8-bit palette indices: the real output of the software renderer
	f = Oracle_Open (s->path, "pgm");
	fprintf (f, "P5\n%d %d\n255\n", vid.width, vid.height);
	for (y=0 ; y<vid.height ; y++)
		fwrite (vid.buffer + y*vid.rowbytes, 1, vid.width, f);
	fclose (f);

// RGB through the palette: the base palette for the 3-D stage (the index
// buffer before any V_UpdatePalette colour shift), the shifted palette the
// "hardware" holds for the composited stage
	pal = stage ? oracle_palette : host_basepal;
	f = Oracle_Open (s->path, "ppm");
	fprintf (f, "P6\n%d %d\n255\n", vid.width, vid.height);
	for (y=0 ; y<vid.height ; y++)
	{
		row = vid.buffer + y*vid.rowbytes;
		for (x=0 ; x<vid.width ; x++)
		{
			rgb[0] = pal[row[x]*3+0];
			rgb[1] = pal[row[x]*3+1];
			rgb[2] = pal[row[x]*3+2];
			fwrite (rgb, 1, 3, f);
		}
	}
	fclose (f);

// the entities the frame had on its list (the player's own entity is never
// drawn, R_DrawEntitiesOnList); static entities joined via R_StoreEfrags
	f = Oracle_Open (s->path, "ents");
	fprintf (f, "# model origin[3] angles[3] frame skinnum syncbase effects kind\n");
	n = 0;
	for (i=0 ; i<cl_numvisedicts ; i++)
	{
		e = cl_visedicts[i];
		if (e == &cl_entities[cl.viewentity] || !e->model)
			continue;
		fprintf (f, "%s %.9g %.9g %.9g %.9g %.9g %.9g %d %d %.9g %d %s\n",
			Oracle_ModelName (e), e->origin[0], e->origin[1], e->origin[2],
			e->angles[0], e->angles[1], e->angles[2], e->frame, e->skinnum,
			e->syncbase, e->effects,
			(e >= cl_static_entities && e < cl_static_entities + MAX_STATIC_ENTITIES) ? "static" : "dynamic");
		n++;
	}
	fclose (f);

	f = Oracle_Open (s->path, "json");
	fprintf (f, "{\n");
	fprintf (f, "  \"map\": \"%s\",\n", cl.worldmodel ? cl.worldmodel->name : "");
	fprintf (f, "  \"stage\": \"%s\",\n", stage ? "full" : "view");
	fprintf (f, "  \"width\": %d,\n  \"height\": %d,\n  \"aspect\": %.9g,\n", vid.width, vid.height, vid.aspect);
	fprintf (f, "  \"vrect\": [%d, %d, %d, %d],\n", r_refdef.vrect.x, r_refdef.vrect.y, r_refdef.vrect.width, r_refdef.vrect.height);
	fprintf (f, "  \"fov_x\": %.9g,\n  \"fov_y\": %.9g,\n", r_refdef.fov_x, r_refdef.fov_y);
	Oracle_WriteVec (f, "vieworg", r_refdef.vieworg);
	Oracle_WriteVec (f, "viewangles", r_refdef.viewangles);
	Oracle_WriteVec (f, "natural_vieworg", natural_org);
	Oracle_WriteVec (f, "natural_viewangles", natural_ang);
	fprintf (f, "  \"view_pinned\": %s,\n", s->has_view ? "true" : "false");
	fprintf (f, "  \"time\": %.17g,\n", used_time);
	fprintf (f, "  \"natural_time\": %.17g,\n", natural_time);
	fprintf (f, "  \"time_pinned\": %s,\n", s->has_time ? "true" : "false");
	fprintf (f, "  \"frame\": %d,\n", rendered);
	fprintf (f, "  \"sv_time\": %.17g,\n", sv.time);
	fprintf (f, "  \"viewsize\": %.9g,\n  \"fov\": %.9g,\n", scr_viewsize.value, scr_fov.value);
	fprintf (f, "  \"r_drawentities\": %.9g,\n  \"r_drawviewmodel\": %.9g,\n", r_drawentities.value, r_drawviewmodel.value);
	fprintf (f, "  \"viewleaf_contents\": %d,\n  \"dowarp\": %d,\n", r_viewleaf ? r_viewleaf->contents : 0, (int)r_dowarp);
	for (i=0, x=0 ; i<MAX_DLIGHTS ; i++)
		if (cl_dlights[i].die >= cl.time && cl_dlights[i].radius > 0)
			x++;
	fprintf (f, "  \"active_dlights\": %d,\n", x);
	// the lights themselves, as R_PushDlights sees them (quaketool view --dlight)
	fprintf (f, "  \"dlights\": [");
	for (i=0, x=0 ; i<MAX_DLIGHTS ; i++)
		if (cl_dlights[i].die >= cl.time && cl_dlights[i].radius > 0)
			fprintf (f, "%s[%.9g, %.9g, %.9g, %.9g, %.9g]", x++ ? ", " : "",
				cl_dlights[i].origin[0], cl_dlights[i].origin[1], cl_dlights[i].origin[2],
				cl_dlights[i].radius, cl_dlights[i].minlight);
	fprintf (f, "],\n");
	fprintf (f, "  \"entities\": %d,\n", n);
	if (bench_frames)
		fprintf (f, "  \"bench_frames\": %d,\n  \"bench_ms\": %.6f,\n", bench_frames, bench_ms);
	fprintf (f, "  \"viewmodel\": {\"model\": \"%s\", \"frame\": %d, \"origin\": [%.9g, %.9g, %.9g], \"angles\": [%.9g, %.9g, %.9g]},\n",
		Oracle_ModelName (&cl.viewent), cl.viewent.frame,
		cl.viewent.origin[0], cl.viewent.origin[1], cl.viewent.origin[2],
		cl.viewent.angles[0], cl.viewent.angles[1], cl.viewent.angles[2]);
	fprintf (f, "  \"realtime\": %.17g,\n  \"host_time\": %.17g,\n", realtime, host_time);
	fprintf (f, "  \"centertime_start\": %.9g,\n  \"centertime_off\": %.9g,\n", scr_centertime_start, scr_centertime_off);
	fprintf (f, "  \"con_current\": %.9g,\n  \"key_dest\": %d,\n  \"m_state\": %d,\n", scr_con_current, (int)key_dest, m_state);
	fprintf (f, "  \"sb_lines\": %d,\n  \"intermission\": %d,\n  \"completed_time\": %.9g,\n", sb_lines, cl.intermission, cl.completed_time);
	fprintf (f, "  \"faceanimtime\": %.9g,\n  \"paused\": %d,\n  \"items\": %d,\n", cl.faceanimtime, (int)cl.paused, cl.items);
	fprintf (f, "  \"scr_vrect\": [%d, %d, %d, %d],\n", scr_vrect.x, scr_vrect.y, scr_vrect.width, scr_vrect.height);
	fprintf (f, "  \"stats\": [");
	for (i=0 ; i<MAX_CL_STATS ; i++)
		fprintf (f, "%s%d", i ? ", " : "", cl.stats[i]);
	fprintf (f, "],\n");
	fprintf (f, "  \"lightstyles\": [");
	for (i=0 ; i<MAX_LIGHTSTYLES ; i++)
		fprintf (f, "%s%d", i ? ", " : "", d_lightstylevalue[i]);
	fprintf (f, "]\n}\n");
	fclose (f);

	Sys_Printf ("oracle: wrote %s.{pgm,ppm,json,ents} (%dx%d, t=%.3f)\n", s->path, vid.width, vid.height, used_time);
}

static void Oracle_MaybeExit (void)
{
	if (exit_when_done && numshots && curshot == numshots && !full_pending)
		Sys_Quit ();
}

static double Oracle_WallTime (void)
{
	struct timespec	ts;

	clock_gettime (CLOCK_MONOTONIC, &ts);
	return ts.tv_sec + ts.tv_nsec * 1e-9;
}

// R_StoreEfrags appends the static entities to cl_visedicts on every
// R_RenderView, so each repeat starts from the list CL_RelinkEntities built.
static void Oracle_Bench (int n)
{
	int		i, numvis;
	double	t0;

	numvis = cl_numvisedicts;
	t0 = Oracle_WallTime ();
	for (i=0 ; i<n ; i++)
	{
		cl_numvisedicts = numvis;
		__real_R_RenderView ();
	}
	bench_ms = (Oracle_WallTime () - t0) * 1000.0 / n;
	bench_frames = n;
	Sys_Printf ("oracle: bench %d warm frames -> %.4f ms/frame (%.1f fps)\n", n, bench_ms, 1000.0 / bench_ms);
}

// oracle_blank: the 3-D view rectangle as one flat palette index, so the
// composited screen is the 2-D layer over a known background.
static void Oracle_Blank (void)
{
	int		y;

	if (oracle_blank.value < 0)
		return;
	for (y=scr_vrect.y ; y<scr_vrect.y + scr_vrect.height ; y++)
		memset (vid.buffer + y*vid.rowbytes + scr_vrect.x, (int)oracle_blank.value & 255, scr_vrect.width);
}

void __wrap_R_RenderView (void)
{
	vec3_t	gunofs;

	active = NULL;
	natural_time = cl.time;
	VectorCopy (r_refdef.vieworg, natural_org);
	VectorCopy (r_refdef.viewangles, natural_ang);

	if (rendered >= settle && curshot < numshots)
		active = &shots[curshot];

	if (active && active->has_view)
	{
	// keep the gun where V_CalcRefdef put it relative to the eye
		VectorSubtract (cl.viewent.origin, r_refdef.vieworg, gunofs);
		VectorCopy (active->org, r_refdef.vieworg);
		VectorCopy (active->ang, r_refdef.viewangles);
		VectorAdd (active->org, gunofs, cl.viewent.origin);
		cl.viewent.angles[YAW] = active->ang[YAW];
		cl.viewent.angles[PITCH] = -active->ang[PITCH];
		cl.viewent.angles[ROLL] = active->ang[ROLL];
	}
	if (active && active->has_time)
		cl.time = active->time;

	__real_R_RenderView ();
	used_time = cl.time;
	Oracle_Blank ();

	bench_frames = 0;
	if (active && oracle_bench.value > 0)
		Oracle_Bench ((int)oracle_bench.value);

	if (active)
	{
		if (active->stage)
			full_pending = true;
		else
		{
			Oracle_Dump (active, 0);
			curshot++;
		}
	}
	cl.time = natural_time;
	rendered++;
}

void Oracle_VidUpdate (void)
{
	if (full_pending)
	{
		Oracle_Dump (&shots[curshot], 1);
		curshot++;
		full_pending = false;
	}
	Oracle_MaybeExit ();

// timedemo: CL_FinishTimeDemo has printed its "frames / seconds / fps" line
	if (cls.timedemo)
		timedemo_seen = true;
	else if (timedemo_seen && exit_when_done)
		Sys_Quit ();
}

//=============================================================================
//
// TEXTURED SPANS
//
// The portable C build only has D_DrawSpans8 (d_init.c picks D_DrawSpans16, an
// asm-only routine, just under id386), so the 1996 x86 binary with its default
// d_subdiv16 1 drew walls with 16-pixel perspective segments where this build
// draws 8. d_init.c's reference to D_DrawSpans8 is wrapped too; oracle_spans
// picks the routine per frame.
//
//=============================================================================

cvar_t	oracle_spans = {"oracle_spans", "8"};

void __real_D_DrawSpans8 (espan_t *pspan);

/*
=============
Oracle_DrawSpansN

D_DrawSpans8 (d_scan.c) with the segment length a parameter: exact s/z, t/z,
1/z every `seg` pixels, affine in between; the last segment steps by division
to its final pixel. seg 16 is the algorithm of d_draw16.s's D_DrawSpans16 in
portable C arithmetic (the asm keeps x87 intermediates and rounds with fistp
under the chop mode R_RenderView_ sets, so rare texels may still differ).
=============
*/
static void Oracle_DrawSpansN (espan_t *pspan, int seg, int segshift)
{
	int				count, spancount;
	unsigned char	*pbase, *pdest;
	fixed16_t		s, t, snext, tnext, sstep, tstep;
	float			sdivz, tdivz, zi, z, du, dv, spancountminus1;
	float			sdivzsegstepu, tdivzsegstepu, zisegstepu;

	sstep = 0;
	tstep = 0;

	pbase = (unsigned char *)cacheblock;

	sdivzsegstepu = d_sdivzstepu * seg;
	tdivzsegstepu = d_tdivzstepu * seg;
	zisegstepu = d_zistepu * seg;

	do
	{
		pdest = (unsigned char *)((byte *)d_viewbuffer +
				(screenwidth * pspan->v) + pspan->u);

		count = pspan->count;

		du = (float)pspan->u;
		dv = (float)pspan->v;

		sdivz = d_sdivzorigin + dv*d_sdivzstepv + du*d_sdivzstepu;
		tdivz = d_tdivzorigin + dv*d_tdivzstepv + du*d_tdivzstepu;
		zi = d_ziorigin + dv*d_zistepv + du*d_zistepu;
		z = (float)0x10000 / zi;

		s = (int)(sdivz * z) + sadjust;
		if (s > bbextents)
			s = bbextents;
		else if (s < 0)
			s = 0;

		t = (int)(tdivz * z) + tadjust;
		if (t > bbextentt)
			t = bbextentt;
		else if (t < 0)
			t = 0;

		do
		{
			if (count >= seg)
				spancount = seg;
			else
				spancount = count;

			count -= spancount;

			if (count)
			{
				sdivz += sdivzsegstepu;
				tdivz += tdivzsegstepu;
				zi += zisegstepu;
				z = (float)0x10000 / zi;

				snext = (int)(sdivz * z) + sadjust;
				if (snext > bbextents)
					snext = bbextents;
				else if (snext < seg)
					snext = seg;

				tnext = (int)(tdivz * z) + tadjust;
				if (tnext > bbextentt)
					tnext = bbextentt;
				else if (tnext < seg)
					tnext = seg;

				sstep = (snext - s) >> segshift;
				tstep = (tnext - t) >> segshift;
			}
			else
			{
				spancountminus1 = (float)(spancount - 1);
				sdivz += d_sdivzstepu * spancountminus1;
				tdivz += d_tdivzstepu * spancountminus1;
				zi += d_zistepu * spancountminus1;
				z = (float)0x10000 / zi;
				snext = (int)(sdivz * z) + sadjust;
				if (snext > bbextents)
					snext = bbextents;
				else if (snext < seg)
					snext = seg;

				tnext = (int)(tdivz * z) + tadjust;
				if (tnext > bbextentt)
					tnext = bbextentt;
				else if (tnext < seg)
					tnext = seg;

				if (spancount > 1)
				{
					sstep = (snext - s) / (spancount - 1);
					tstep = (tnext - t) / (spancount - 1);
				}
			}

			do
			{
				*pdest++ = *(pbase + (s >> 16) + (t >> 16) * cachewidth);
				s += sstep;
				t += tstep;
			} while (--spancount > 0);

			s = snext;
			t = tnext;

		} while (count > 0);

	} while ((pspan = pspan->pnext) != NULL);
}

/*
=============
Oracle_DrawSpansExact

Every pixel perspective-correct: s = s/z / (1/z) at the pixel itself, clamped
to the surface block like D_DrawSpans8's first pixel. Not an id routine --
it answers "how much of the diff is the affine segments".
=============
*/
static void Oracle_DrawSpansExact (espan_t *pspan)
{
	int				count;
	unsigned char	*pbase, *pdest;
	fixed16_t		s, t;
	float			sdivz, tdivz, zi, z, du, dv;

	pbase = (unsigned char *)cacheblock;

	do
	{
		pdest = (unsigned char *)((byte *)d_viewbuffer +
				(screenwidth * pspan->v) + pspan->u);
		count = pspan->count;
		du = (float)pspan->u;
		dv = (float)pspan->v;

		do
		{
			sdivz = d_sdivzorigin + dv*d_sdivzstepv + du*d_sdivzstepu;
			tdivz = d_tdivzorigin + dv*d_tdivzstepv + du*d_tdivzstepu;
			zi = d_ziorigin + dv*d_zistepv + du*d_zistepu;
			z = (float)0x10000 / zi;

			s = (int)(sdivz * z) + sadjust;
			if (s > bbextents)
				s = bbextents;
			else if (s < 0)
				s = 0;

			t = (int)(tdivz * z) + tadjust;
			if (t > bbextentt)
				t = bbextentt;
			else if (t < 0)
				t = 0;

			*pdest++ = *(pbase + (s >> 16) + (t >> 16) * cachewidth);
			du += 1;
		} while (--count > 0);

	} while ((pspan = pspan->pnext) != NULL);
}

void __wrap_D_DrawSpans8 (espan_t *pspan)
{
	switch ((int)oracle_spans.value)
	{
	case 16:
		Oracle_DrawSpansN (pspan, 16, 4);
		break;
	case 1:
		Oracle_DrawSpansExact (pspan);
		break;
	default:
		__real_D_DrawSpans8 (pspan);
		break;
	}
}
