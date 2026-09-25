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
// vid_oracle.c -- the quake-rust oracle's video driver: id's vid_null.c at a
// chosen resolution (-width/-height, default 320x200), with the buffers sized
// the way the real drivers size them (D_SurfaceCacheForRes, as vid_x.c /
// vid_win.c do) and VID_Update handing the finished frame to oracle.c.
//
// vid.aspect is 1.0 (square pixels), as in vid_null.c; -oracle_aspect <f>
// overrides it (vid_win.c / vid_vga.c use height/width * 320/240, i.e. 0.8333
// at 320x200 -- the DOS/Win 320x200 mode stretched onto a 4:3 monitor).

#include "quakedef.h"
#include "d_local.h"

viddef_t	vid;				// global video state

unsigned short	d_8to16table[256];
unsigned	d_8to24table[256];

byte		oracle_palette[768];	// the palette most recently sent to the "hardware"

void Oracle_Init (void);
void Oracle_VidUpdate (void);

void M_DrawPic (int x, int y, qpic_t *pic);
void M_Menu_Options_f (void);
extern void (*vid_menudrawfn)(void);
extern void (*vid_menukeyfn)(int key);
extern cvar_t bgmvolume, volume;

// The video menu: the title every id driver's VID_MenuDraw starts with
// (vid_win.c, vid_dos.c), and Escape back to Options as their VID_MenuKey
// does. Without a vid_menudrawfn menu.c hides the Options screen's "Video
// Options" row (vid_null.c has none); the mode list below the title is the
// driver's own and is not drawn.
static void Oracle_VidMenuDraw (void)
{
	qpic_t	*p;

	p = Draw_CachePic ("gfx/vidmodes.lmp");
	M_DrawPic ((320-p->width)/2, 4, p);
}

static void Oracle_VidMenuKey (int key)
{
	if (key == K_ESCAPE)
	{
		S_LocalSound ("misc/menu1.wav");
		M_Menu_Options_f ();
	}
}

void	VID_SetPalette (unsigned char *palette)
{
	memcpy (oracle_palette, palette, 768);
}

void	VID_ShiftPalette (unsigned char *palette)
{
	memcpy (oracle_palette, palette, 768);
}

void	VID_Init (unsigned char *palette)
{
	int		i, w, h, surfcachesize;
	byte	*surfcache;

	w = 320;
	h = 200;
	i = COM_CheckParm ("-width");
	if (i && i < com_argc-1)
		w = Q_atoi (com_argv[i+1]);
	i = COM_CheckParm ("-height");
	if (i && i < com_argc-1)
		h = Q_atoi (com_argv[i+1]);
	if (w < 320 || h < 200 || w > MAXWIDTH || h > MAXHEIGHT)
		Sys_Error ("VID_Init: unsupported mode %dx%d (320x200 .. %dx%d)", w, h, MAXWIDTH, MAXHEIGHT);

	vid.width = vid.conwidth = w;
	vid.height = vid.conheight = h;
	vid.maxwarpwidth = WARP_WIDTH;
	vid.maxwarpheight = WARP_HEIGHT;
	vid.aspect = 1.0;
	i = COM_CheckParm ("-oracle_aspect");
	if (i && i < com_argc-1)
		vid.aspect = Q_atof (com_argv[i+1]);
	vid.numpages = 1;
	vid.colormap = host_colormap;
	vid.fullbright = 256 - LittleLong (*((int *)vid.colormap + 2048));
	vid.buffer = vid.conbuffer = malloc (w * h);
	vid.rowbytes = vid.conrowbytes = w;
	vid.recalc_refdef = 1;

	d_pzbuffer = malloc (w * h * sizeof (*d_pzbuffer));
	surfcachesize = D_SurfaceCacheForRes (w, h);
	surfcache = malloc (surfcachesize);
	if (!vid.buffer || !d_pzbuffer || !surfcache)
		Sys_Error ("VID_Init: out of memory");
	D_InitCaches (surfcache, surfcachesize);

	VID_SetPalette (palette);
	vid_menudrawfn = Oracle_VidMenuDraw;
	// snd_null.c defines the two volume cvars but, having no S_Init, never
	// registers them: the Options sliders would read 0 instead of their
	// defaults (0.7, 1) that snd_dma.c's S_Init registers
	Cvar_RegisterVariable (&bgmvolume);
	Cvar_RegisterVariable (&volume);
	vid_menukeyfn = Oracle_VidMenuKey;
	Oracle_Init ();
}

void	VID_Shutdown (void)
{
}

void	VID_Update (vrect_t *rects)
{
	Oracle_VidUpdate ();
}

/*
================
D_BeginDirectRect
================
*/
void D_BeginDirectRect (int x, int y, byte *pbitmap, int width, int height)
{
}


/*
================
D_EndDirectRect
================
*/
void D_EndDirectRect (int x, int y, int width, int height)
{
}
