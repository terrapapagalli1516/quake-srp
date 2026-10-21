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
// particle_oracle.c -- QuakeC's particle() calls made through id's own
// PF_particle, and a log of every svc_particle id's client parses
// (oracle/particle_wire.py makes the same calls through the port,
// `quaketool partwire`, and compares the logs).
//
//   oracle_particles script log   from the first server frame the local client
//                                 is spawned in, make the script's calls through
//                                 PF_particle, one a frame; log every svc_particle
//                                 the client parses; quit the frame after the last
//
// A script line is one call's arguments, "ox oy oz dx dy dz color count" ('#'
// starts a comment), set in the PARM globals as QuakeC's call sets them. The
// calls are made after SV_Physics (wrapped at link time, build.sh): a console
// command runs before Host_ServerFrame's SV_ClearDatagram, which would drop
// what it wrote, while SV_SendClientMessages sends what is written here.
//
// The log wraps R_ParseParticleEffect: the wrapper reads the message as id's
// does (r_part.c), writes a line, puts the read position back and calls it.
// A line: "particle ox oy oz dx dy dz color count", as R_ParseParticleEffect
// read them, the count after its 255 -> 1024.

#include "quakedef.h"

void PF_particle (void);
void __real_SV_Physics (void);
void __real_R_ParseParticleEffect (void);

typedef struct
{
	vec3_t		org, dir;
	float		color, count;
} partcall_t;

#define MAX_PART_CALLS	256
static partcall_t	calls[MAX_PART_CALLS];
static int			numcalls, nextcall;
static qboolean		calling;
static FILE			*partlog;

static void Oracle_Particles_f (void)
{
	FILE		*f;
	char		line[256], *comment;
	partcall_t	c;
	int			n;

	if (Cmd_Argc () != 3)
	{
		Con_Printf ("oracle_particles script log\n");
		return;
	}
	f = fopen (Cmd_Argv (1), "r");
	if (!f)
		Sys_Error ("oracle_particles: cannot read %s", Cmd_Argv (1));
	numcalls = 0;
	while (fgets (line, sizeof(line), f))
	{
		comment = strchr (line, '#');
		if (comment)
			*comment = 0;
		n = sscanf (line, "%f %f %f %f %f %f %f %f", &c.org[0], &c.org[1], &c.org[2],
			&c.dir[0], &c.dir[1], &c.dir[2], &c.color, &c.count);
		if (n == EOF)
			continue;	// a blank line
		if (n != 8)
			Sys_Error ("oracle_particles: want \"ox oy oz dx dy dz color count\": %s", line);
		if (numcalls == MAX_PART_CALLS)
			Sys_Error ("oracle_particles: more than %d calls", MAX_PART_CALLS);
		calls[numcalls++] = c;
	}
	fclose (f);
	partlog = fopen (Cmd_Argv (2), "w");
	if (!partlog)
		Sys_Error ("oracle_particles: cannot write %s", Cmd_Argv (2));
	nextcall = 0;
	calling = true;
}

void Oracle_Particles_Init (void)
{
	Cmd_AddCommand ("oracle_particles", Oracle_Particles_f);
}

void __wrap_SV_Physics (void)
{
	partcall_t	*c;

	__real_SV_Physics ();
	if (!calling || !svs.clients || !svs.clients[0].spawned)
		return;
	if (nextcall == numcalls)
	{
		// the last call's message reached the client in the frame before
		fclose (partlog);
		partlog = NULL;
		Sys_Quit ();
	}
	c = &calls[nextcall++];
	VectorCopy (c->org, G_VECTOR(OFS_PARM0));
	VectorCopy (c->dir, G_VECTOR(OFS_PARM1));
	G_FLOAT(OFS_PARM2) = c->color;
	G_FLOAT(OFS_PARM3) = c->count;
	PF_particle ();
}

void __wrap_R_ParseParticleEffect (void)
{
	int			readcount, msgcount, color, i;
	qboolean	badread;
	vec3_t		org, dir;

	if (partlog)
	{
		readcount = msg_readcount;
		badread = msg_badread;
		for (i=0 ; i<3 ; i++)
			org[i] = MSG_ReadCoord ();
		for (i=0 ; i<3 ; i++)
			dir[i] = MSG_ReadChar () * (1.0/16);
		msgcount = MSG_ReadByte ();
		color = MSG_ReadByte ();
		fprintf (partlog, "particle %.4f %.4f %.4f %.4f %.4f %.4f %d %d\n", org[0], org[1], org[2],
			dir[0], dir[1], dir[2], color, msgcount == 255 ? 1024 : msgcount);
		msg_readcount = readcount;
		msg_badread = badread;
	}
	__real_R_ParseParticleEffect ();
}
