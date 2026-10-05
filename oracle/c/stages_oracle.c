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
// stages_oracle.c -- the stages of one frame of id's renderer, for
// oracle/pixel_trace.py to put beside the port's (quaketool view --stages).
//
//   oracle_stages path      the next shot also writes, to path, a record per line:
//     F ...                 the frame: vpn/vright/vup, R_ViewChanged's numbers, the
//                           screenedge and view_clipplanes planes, skytime
//     E v u u_step s0 s1 nearzi last   every edge R_ScanEdges starts with (row v,
//                           its surfaces' ids, the last row it is active on)
//     S id key flags nearzi ziorigin zistepu zistepv   every surface with a span
//     G id kind mip ...     what D_CalcGradients left a textured surface's span
//                           routine: the s/z and t/z planes, sadjust, tadjust,
//                           bbextents, bbextentt, and a hash of the block it reads
//     P v u count id        every span
// A float is its IEEE bits in hex (both sides print them so: equal means equal
// to the bit). A surface's id is its model ("world", "*N", "ext@x,y,z") and its
// face's index in the model's surfaces. The spans of a frame can be drawn in
// several batches (R_ScanEdges flushes D_DrawSurfaces when its span buffer
// fills): the records repeat per batch, and the script takes the union.
//
// The links wrap R_ScanEdges and D_DrawSurfaces (-Wl,--wrap); oracle.c's span
// wraps call Oracle_StagesGrads.

#include "quakedef.h"
#include "r_local.h"
#include "d_local.h"

extern float	skytime;
extern edge_t	*newedges[MAXHEIGHT];
extern edge_t	*removeedges[MAXHEIGHT];
extern vec3_t	transformed_modelorg;

static char		stages_path[1024];
static FILE		*stages;	// open while the shot frame renders

void __real_R_ScanEdges (void);
void __real_D_DrawSurfaces (void);

static void Oracle_Stages_f (void)
{
	if (Cmd_Argc () != 2)
	{
		Con_Printf ("oracle_stages path\n");
		return;
	}
	Q_strncpy (stages_path, Cmd_Argv (1), sizeof(stages_path) - 1);
}

void Oracle_StagesInit (void)
{
	Cmd_AddCommand ("oracle_stages", Oracle_Stages_f);
}

static void F32 (char *tag, float f)
{
	unsigned	u;

	memcpy (&u, &f, 4);
	fprintf (stages, " %s=%08x", tag, u);
}

static void Vec (char *tag, float *v)
{
	char	name[32];
	int		i;

	for (i=0 ; i<3 ; i++)
	{
		sprintf (name, "%s%d", tag, i);
		F32 (name, v[i]);
	}
}

// The surface's id: its model and its face's index in the model's surfaces.
static char *Oracle_SurfId (surf_t *s)
{
	static char	buf[128];
	entity_t	*e;
	model_t		*m;
	int			face;

	if (s->flags & SURF_DRAWBACKGROUND)
		return "bg";
	e = s->entity;
	m = e->model;
	face = (msurface_t *)s->data - m->surfaces;
	if (e == &cl_entities[0])
		sprintf (buf, "world:%d", face);
	else if (m->name[0] == '*')
		sprintf (buf, "%s:%d", m->name, face);
	else
		sprintf (buf, "ext@%d,%d,%d:%d", (int)e->origin[0], (int)e->origin[1], (int)e->origin[2], face);
	return buf;
}

// FNV-1a over a block's rows
static unsigned Oracle_Hash (byte *p, int w, int h, int stride)
{
	unsigned	hash;
	int			x, y;

	hash = 2166136261u;
	for (y=0 ; y<h ; y++)
		for (x=0 ; x<w ; x++)
		{
			hash ^= p[y*stride + x];
			hash *= 16777619u;
		}
	return hash;
}

void Oracle_StagesBegin (void)
{
	if (!stages_path[0])
		return;
	stages = fopen (stages_path, "w");
	if (!stages)
		Sys_Error ("oracle_stages: cannot write %s", stages_path);
	stages_path[0] = 0;
}

void Oracle_StagesEnd (void)
{
	int		i;

	if (!stages)
		return;
	fprintf (stages, "F");
	Vec ("vpn", base_vpn);
	Vec ("vright", base_vright);
	Vec ("vup", base_vup);
	F32 ("xcenter", xcenter);
	F32 ("ycenter", ycenter);
	F32 ("xscale", xscale);
	F32 ("yscale", yscale);
	F32 ("xscaleinv", xscaleinv);
	F32 ("yscaleinv", yscaleinv);
	F32 ("hfov", r_refdef.horizontalFieldOfView);
	F32 ("skytime", skytime);
	for (i=0 ; i<4 ; i++)
	{
		char	name[32];

		sprintf (name, "edge%d_", i);
		Vec (name, screenedge[i].normal);
		sprintf (name, "clip%d_", i);
		Vec (name, view_clipplanes[i].normal);
		sprintf (name, "clip%d_dist", i);
		F32 (name, view_clipplanes[i].dist);
	}
	fprintf (stages, "\n");
	fclose (stages);
	stages = NULL;
}

void __wrap_R_ScanEdges (void)
{
	int		v, *last;
	edge_t	*e;

	if (stages)
	{
		last = malloc ((edge_p - r_edges) * sizeof(int));
		for (v=0 ; v<r_refdef.vrect.height ; v++)
			for (e = removeedges[v] ; e ; e = e->nextremove)
				last[e - r_edges] = v;
		for (v=0 ; v<r_refdef.vrect.height ; v++)
			for (e = newedges[v] ; e ; e = e->next)
			{
				fprintf (stages, "E %d %d %d %s", v, e->u, e->u_step,
						e->surfs[0] ? Oracle_SurfId (&surfaces[e->surfs[0]]) : "-");
				fprintf (stages, " %s", e->surfs[1] ? Oracle_SurfId (&surfaces[e->surfs[1]]) : "-");
				F32 ("nearzi", e->nearzi);
				fprintf (stages, " last=%d\n", last[e - r_edges]);
			}
		free (last);
	}
	__real_R_ScanEdges ();
}

void __wrap_D_DrawSurfaces (void)
{
	surf_t	*s;
	espan_t	*sp;

	if (stages)
		for (s = &surfaces[1] ; s<surface_p ; s++)
		{
			if (!s->spans)
				continue;
			fprintf (stages, "S %s key=%d flags=%d", Oracle_SurfId (s), s->key, s->flags);
			F32 ("nearzi", s->nearzi);
			F32 ("ziorigin", s->d_ziorigin);
			F32 ("zistepu", s->d_zistepu);
			F32 ("zistepv", s->d_zistepv);
			fprintf (stages, "\n");
			for (sp = s->spans ; sp ; sp = sp->pnext)
				fprintf (stages, "P %d %d %d %s\n", sp->v, sp->u, sp->count, Oracle_SurfId (s));
		}
	__real_D_DrawSurfaces ();
}

// A textured surface's span routine is about to draw pspan, its spans:
// D_CalcGradients' results and the block, for the surface whose list it is.
void Oracle_StagesGrads (espan_t *pspan, char *kind)
{
	surf_t		*s;
	msurface_t	*pface;
	int			h, mip;

	if (!stages)
		return;
	for (s = &surfaces[1] ; s<surface_p ; s++)
		if (s->spans == pspan)
			break;
	if (s == surface_p)
		return;
	h = (bbextentt + 1) >> 16;
	if (h > 64 && kind[0] == 't')
		h = 64;		// a liquid reads its 64x64 texture, not 16384 rows
	// d_edge.c's miplevel is static: the one whose extents give bbextents
	pface = s->data;
	for (mip=0 ; mip<3 ; mip++)
		if (((pface->extents[0] << 16) >> mip) - 1 == bbextents)
			break;
	fprintf (stages, "G %s %s mip=%d", Oracle_SurfId (s), kind, mip);
	Vec ("tmo", transformed_modelorg);
	F32 ("sdivzorigin", d_sdivzorigin);
	F32 ("sdivzstepu", d_sdivzstepu);
	F32 ("sdivzstepv", d_sdivzstepv);
	F32 ("tdivzorigin", d_tdivzorigin);
	F32 ("tdivzstepu", d_tdivzstepu);
	F32 ("tdivzstepv", d_tdivzstepv);
	fprintf (stages, " sadjust=%d tadjust=%d bbextents=%d bbextentt=%d block=%dx%d hash=%08x\n",
			sadjust, tadjust, bbextents, bbextentt, cachewidth, h,
			Oracle_Hash ((byte *)cacheblock, cachewidth, h, cachewidth));
}
